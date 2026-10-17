package com.skadi.core

import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.withContext
import kotlinx.serialization.builtins.ListSerializer
import kotlinx.serialization.json.Json
import okhttp3.OkHttpClient
import okhttp3.Request

/**
 * Film and episode downloader (SKADI-T-0637), the video twin of [Downloader]:
 * poster, subtitle files and skip markers first (each best-effort), then the
 * video itself through [ResumableFetch], then `meta.json` last.
 *
 * The bytes come from the same route the player streams from
 * (`…/video`, which answers Range: SKADI-T-0635), so the server needs nothing new.
 */
class VideoDownloader(
    private val api: SkadiApi,
    private val store: OfflineVideoStore,
    private val client: OkHttpClient = downloadClient(),
    private val now: () -> Long = System::currentTimeMillis,
) {
    private val json = Json { ignoreUnknownKeys = true }

    suspend fun download(
        req: VideoDownloadRequest,
        onProgress: (Float) -> Unit,
    ): Result<Unit> = withContext(Dispatchers.IO) {
        runCatching {
            val key = req.key
            val job = coroutineContext[Job]
            val url = if (req.kind == "movie") {
                api.movieVideoUrl(req.parentId, req.itemId)
            } else {
                api.episodeVideoUrl(req.parentId, req.itemId)
            }
            store.dir(key).mkdirs()
            req.posterUrl?.takeIf { it.isNotEmpty() }?.let { saveTo(it, store.posterFile(key)) }
            val subs = runCatching { getJson(Subtitles.listUrl(url)) { json.decodeFromString(ListSerializer(SubtitleTrack.serializer()), it) } }
                .getOrNull().orEmpty()
                .filter { t -> saveTo(Subtitles.fileUrl(url, t.index), store.subtitleFile(key, t)) }
            val markers = Markers.url(url)?.let { u ->
                runCatching { getJson(u) { json.decodeFromString(SkipMarkers.serializer(), it) } }.getOrNull()
            } ?: SkipMarkers()

            ResumableFetch.fetch(
                client = client,
                url = url,
                authHeader = api.authHeader(),
                part = store.partFile(key),
                target = store.videoFile(key),
                onProgress = onProgress,
                isCancelled = { job?.isActive == false },
                what = "video",
            )
            store.saveMeta(
                OfflineVideo(
                    key = key,
                    kind = req.kind,
                    parentId = req.parentId,
                    itemId = req.itemId,
                    title = req.title,
                    subtitle = req.subtitle,
                    season = req.season,
                    number = req.number,
                    posterUrl = req.posterUrl,
                    sizeBytes = store.videoFile(key).length(),
                    subtitles = subs,
                    markers = markers,
                    downloadedAt = now(),
                ),
            )
            onProgress(1f)
        }
    }

    private fun <T> getJson(url: String, parse: (String) -> T): T? =
        client.newCall(Request.Builder().url(url).build()).execute().use { r ->
            if (r.isSuccessful) r.body?.string()?.let(parse) else null
        }

    /** Save a small file; false when it could not be fetched. */
    private fun saveTo(url: String, file: java.io.File): Boolean = runCatching {
        client.newCall(Request.Builder().url(url).build()).execute().use { r ->
            if (!r.isSuccessful) return@use false
            file.parentFile?.mkdirs()
            r.body?.byteStream()?.use { ins -> file.outputStream().use { ins.copyTo(it) } } != null
        }
    }.getOrDefault(false)
}
