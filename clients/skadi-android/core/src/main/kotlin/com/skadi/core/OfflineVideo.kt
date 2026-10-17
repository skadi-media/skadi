package com.skadi.core

import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json
import java.io.File

/**
 * A film or an episode on the phone (SKADI-T-0637). Same rules as books
 * ([OfflineBook]): app-private storage, no budget, the operator removes what
 * they no longer want. `meta.json` is written last, so its presence means
 * the video is complete.
 *
 * [key] is the [VideoProgress] key (`movie:{movie}:{edition}` or
 * `episode:{series}:{episode}`), so a downloaded copy and a streamed one share
 * one saved position.
 */
@Serializable
data class OfflineVideo(
    val key: String,
    /** `movie` or `episode`. */
    val kind: String,
    /** Movie id, or series id. */
    @SerialName("parent_id") val parentId: String,
    /** Edition id, or episode id. */
    @SerialName("item_id") val itemId: String,
    /** Film title, or series title. */
    val title: String,
    /** Year for a film; `S01E02 · Title` for an episode. */
    val subtitle: String? = null,
    val season: Int? = null,
    val number: Int? = null,
    /** The server's poster URL, for watch records; the art itself is `poster.jpg`. */
    @SerialName("poster_url") val posterUrl: String? = null,
    @SerialName("size_bytes") val sizeBytes: Long = 0,
    /** Subtitle files saved beside the video. */
    val subtitles: List<SubtitleTrack> = emptyList(),
    val markers: SkipMarkers = SkipMarkers(),
    @SerialName("downloaded_at") val downloadedAt: Long = 0,
) {
    val isMovie get() = kind == "movie"

    /** What Home records when this starts playing ([VideoProgress.start]). */
    fun watchRecord(): WatchRecord = WatchRecord(
        key = key, kind = kind, parentId = parentId, itemId = itemId,
        title = title, subtitle = subtitle, posterUrl = posterUrl,
        season = season, number = number,
    )

    /** The title the player shows. */
    val playerTitle: String
        get() = if (isMovie) {
            subtitle?.let { "$title ($it)" } ?: title
        } else {
            listOfNotNull(title, subtitle?.substringBefore(" · ")).joinToString(" · ")
        }

    companion object {
        fun movieKey(movieId: String, editionId: String) = "movie:$movieId:$editionId"
        fun episodeKey(seriesId: String, episodeId: String) = "episode:$seriesId:$episodeId"
    }
}

/**
 * What the download worker needs to fetch one video: the ids and what to show.
 * Travels as JSON in WorkManager input data. No URL here: the URL carries the
 * API key, and the worker builds it from the paired server.
 */
@Serializable
data class VideoDownloadRequest(
    val kind: String,
    @SerialName("parent_id") val parentId: String,
    @SerialName("item_id") val itemId: String,
    val title: String,
    val subtitle: String? = null,
    val season: Int? = null,
    val number: Int? = null,
    @SerialName("poster_url") val posterUrl: String? = null,
) {
    val key: String
        get() = if (kind == "movie") {
            OfflineVideo.movieKey(parentId, itemId)
        } else {
            OfflineVideo.episodeKey(parentId, itemId)
        }

    companion object {
        fun movie(m: Movie, editionId: String) = VideoDownloadRequest(
            kind = "movie", parentId = m.id, itemId = editionId,
            title = m.title, subtitle = m.year?.toString(), posterUrl = m.poster_url,
        )

        fun episode(s: Series, e: Episode) = VideoDownloadRequest(
            kind = "episode", parentId = s.id, itemId = e.id,
            title = s.title,
            subtitle = listOfNotNull(e.code, e.title).joinToString(" · "),
            season = e.season, number = e.number, posterUrl = s.poster_url,
        )
    }
}

/**
 * On-device video storage (SKADI-T-0637), beside the books:
 * `videos/{key}/{video, video.part, meta.json, poster.jpg, subs/{n}.{fmt}}`.
 * The directory name is the progress key with anything but letters, digits,
 * `-` and `_` replaced by `_`.
 */
class OfflineVideoStore(private val root: File) {
    private val json = Json { ignoreUnknownKeys = true; encodeDefaults = true }

    fun dir(key: String) = File(root, "videos/${dirName(key)}")
    fun videoFile(key: String): File = File(dir(key), "video")
    fun partFile(key: String): File = File(dir(key), "video.part")
    fun posterFile(key: String): File = File(dir(key), "poster.jpg")
    fun subtitleFile(key: String, t: SubtitleTrack): File =
        File(dir(key), "subs/${t.index}.${t.format.filter { it.isLetterOrDigit() }.ifEmpty { "srt" }}")
    private fun metaFile(key: String) = File(dir(key), "meta.json")

    fun isDownloaded(key: String): Boolean = metaFile(key).isFile && videoFile(key).isFile

    fun meta(key: String): OfflineVideo? =
        runCatching { json.decodeFromString<OfflineVideo>(metaFile(key).readText()) }.getOrNull()
            ?.takeIf { videoFile(key).isFile }

    /** Every complete download, films first by title, then episodes in order. */
    fun list(): List<OfflineVideo> =
        File(root, "videos").listFiles().orEmpty().mapNotNull { d ->
            runCatching {
                json.decodeFromString<OfflineVideo>(File(d, "meta.json").readText())
            }.getOrNull()?.takeIf { File(d, "video").isFile }
        }.sortedWith(
            compareBy<OfflineVideo>({ !it.isMovie }, { it.title.lowercase() }, { it.season ?: 0 }, { it.number ?: 0 }),
        )

    /** Bytes held by a download that has not finished (its part file). */
    fun partialBytes(key: String): Long = partFile(key).takeIf { it.isFile }?.length() ?: 0L

    /** Bytes held by every unfinished download. */
    fun partialBytesTotal(): Long =
        File(root, "videos").listFiles().orEmpty().sumOf { d ->
            File(d, "video.part").takeIf { it.isFile }?.length() ?: 0L
        }

    fun saveMeta(meta: OfflineVideo) {
        dir(meta.key).mkdirs()
        val target = metaFile(meta.key)
        val tmp = File(target.parentFile, "meta.json.tmp")
        tmp.writeText(json.encodeToString(OfflineVideo.serializer(), meta))
        if (!tmp.renameTo(target)) {
            target.delete()
            tmp.renameTo(target)
        }
    }

    /** Remove the download, finished or not. */
    fun delete(key: String) {
        dir(key).deleteRecursively()
    }

    companion object {
        fun dirName(key: String): String = key.replace(Regex("[^A-Za-z0-9_-]"), "_")
    }
}

/**
 * The downloaded episode that follows [season]/[number] of [seriesId], in
 * broadcast order, specials skipped: what the player goes to next when there
 * is no network to ask the server (SKADI-T-0637).
 */
fun nextDownloadedEpisode(videos: List<OfflineVideo>, seriesId: String, season: Int, number: Int): OfflineVideo? =
    videos.filter { !it.isMovie && it.parentId == seriesId && (it.season ?: 0) > 0 }
        .sortedWith(compareBy({ it.season ?: 0 }, { it.number ?: 0 }))
        .firstOrNull { (it.season ?: 0) > season || ((it.season ?: 0) == season && (it.number ?: 0) > number) }

/** What the downloads on this phone take up (SKADI-T-0637): books and video together. */
data class StorageUse(val books: Long, val videos: Long, val partial: Long) {
    val total: Long get() = books + videos + partial

    companion object {
        fun of(books: List<OfflineBook>, videos: List<OfflineVideo>, partialBytes: Long = 0): StorageUse =
            StorageUse(
                books = books.sumOf { it.sizeBytes },
                videos = videos.sumOf { it.sizeBytes },
                partial = partialBytes,
            )
    }
}
