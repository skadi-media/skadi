package com.skadi.core

import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import okhttp3.OkHttpClient
import kotlinx.coroutines.Job
import okhttp3.Request
import java.util.concurrent.TimeUnit

/** OkHttp default read timeout is 10s — fine for a request/response, but it
 *  trips on any 10s gap between chunks of a multi-GB streamed audiobook over
 *  slow Wi-Fi, resetting the connection mid-transfer (the download then
 *  "interrupts halfway" and resumes from the .part — SKADI-T-0354). A streamed
 *  download wants NO idle-read cap; keep a sane connect timeout. */
internal fun downloadClient(): OkHttpClient =
    OkHttpClient.Builder()
        .connectTimeout(20, TimeUnit.SECONDS)
        .readTimeout(0, TimeUnit.MILLISECONDS)
        .callTimeout(0, TimeUnit.MILLISECONDS)
        .build()

/**
 * Book downloader (SKADI-T-0342, hardened in the second review pass): audio
 * via the Range endpoint (SKADI-T-0329) with RESUME. The fetch and its
 * integrity rules live in [ResumableFetch], shared with video (SKADI-T-0637).
 * meta.json is written LAST, so its presence means "complete".
 */
class Downloader(
    private val api: SkadiApi,
    private val store: OfflineStore,
    private val client: OkHttpClient = downloadClient(),
) {
    suspend fun download(
        book: Book,
        fid: String,
        onProgress: (Float) -> Unit,
    ): Result<Unit> = withContext(Dispatchers.IO) {
        runCatching {
            store.saveChapters(
                fid,
                runCatching { api.chapters(book.id, fid) }.getOrDefault(emptyList()),
            )
            book.coverUrl?.takeIf { it.isNotEmpty() }?.let { url ->
                runCatching {
                    client.newCall(Request.Builder().url(url).build()).execute().use { r ->
                        if (r.isSuccessful) r.body?.byteStream()?.use { ins ->
                            store.coverFile(fid).outputStream().use { ins.copyTo(it) }
                        }
                    }
                }
            }
            val job = coroutineContext[Job]
            fetchAudio(book, fid, onProgress) { job?.isActive == false }
            store.saveMeta(
                OfflineBook(
                    bookId = book.id,
                    fileId = fid,
                    title = book.title,
                    sortTitle = book.sortTitle,
                    authors = book.authors,
                    narrators = book.narrators,
                    seriesName = book.series?.name,
                    seriesPosition = book.series?.position,
                    sizeBytes = store.audioFile(fid).length(),
                ),
            )
            onProgress(1f)
        }
    }

    private fun fetchAudio(
        book: Book,
        fid: String,
        onProgress: (Float) -> Unit,
        isCancelled: () -> Boolean,
    ) = ResumableFetch.fetch(
        client = client,
        url = api.audioUrl(book.id, fid),
        authHeader = api.authHeader(),
        part = store.partFile(fid),
        target = store.audioFile(fid),
        onProgress = onProgress,
        isCancelled = isCancelled,
        what = "audio",
    )
}
