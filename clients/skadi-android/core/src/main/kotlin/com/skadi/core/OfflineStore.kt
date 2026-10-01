package com.skadi.core

import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json
import java.io.File

/**
 * On-device book storage (SKADI-T-0342): one directory per book file under the
 * app's private files dir — `books/{fid}/{audio.m4b, meta.json, chapters.json,
 * cover.jpg}`. The catalog is the directory listing (meta.json presence =
 * fully downloaded; the audio is written to a .part file until complete, so a
 * killed download never masquerades as a finished book).
 */
@Serializable
data class OfflineBook(
    @SerialName("book_id") val bookId: String,
    @SerialName("file_id") val fileId: String,
    val title: String,
    val authors: List<String> = emptyList(),
    val narrators: List<String> = emptyList(),
    @SerialName("series_name") val seriesName: String? = null,
    @SerialName("series_position") val seriesPosition: String? = null,
    @SerialName("size_bytes") val sizeBytes: Long = 0,
    @SerialName("finished") val finished: Boolean = false,
    @SerialName("position_s") val positionS: Double = 0.0,
    /**
     * Hidden from Home's "Keep listening" shelf (SKADI-T-0647). Deliberately a
     * flag and not a position reset: "remove from Home" promises tidying, and
     * someone nine hours into a book should not lose their place to it. The
     * download and the position both survive, so the book is still in the
     * library's Downloaded view and resumes where it was.
     *
     * Defaults false, so meta.json files written before this existed load
     * unchanged.
     */
    @SerialName("hidden_from_home") val hiddenFromHome: Boolean = false,
    /**
     * Playback speed this book was last played at (SKADI-T-0659); null until
     * the listener changes it, which means "the default speed". Kept per book
     * because the right speed depends on the narrator.
     */
    val speed: Float? = null,
)

class OfflineStore(private val root: File) {
    private val json = Json { ignoreUnknownKeys = true; encodeDefaults = true }

    private fun dir(fid: String) = File(root, "books/$fid")
    fun audioFile(fid: String): File = File(dir(fid), "audio.m4b")
    fun partFile(fid: String): File = File(dir(fid), "audio.m4b.part")
    fun chaptersFile(fid: String): File = File(dir(fid), "chapters.json")
    fun coverFile(fid: String): File = File(dir(fid), "cover.jpg")
    private fun metaFile(fid: String) = File(dir(fid), "meta.json")

    fun isDownloaded(fid: String): Boolean =
        metaFile(fid).isFile && audioFile(fid).isFile

    fun list(): List<OfflineBook> =
        File(root, "books").listFiles().orEmpty().mapNotNull { d ->
            runCatching {
                json.decodeFromString<OfflineBook>(File(d, "meta.json").readText())
            }.getOrNull()
        }.sortedBy { it.title.lowercase() }

    /**
     * When this book was last listened to, as epoch millis (0 if unknown).
     *
     * `meta.json` is rewritten only by [updateProgress] during playback and by
     * the download that created it, so its mtime *is* the last-played time —
     * no schema change and every existing download already has one.
     */
    fun lastPlayedAt(fid: String): Long = metaFile(fid).lastModified()

    fun meta(fid: String): OfflineBook? =
        runCatching { json.decodeFromString<OfflineBook>(metaFile(fid).readText()) }.getOrNull()

    fun saveMeta(meta: OfflineBook) {
        // tmp + rename: this is rewritten every 10s of playback — a process
        // kill mid-write must never leave a truncated meta.json (which would
        // drop the book from the shelf while its 2GB audio stays on disk).
        dir(meta.fileId).mkdirs()
        val target = metaFile(meta.fileId)
        val tmp = File(target.parentFile, "meta.json.tmp")
        tmp.writeText(json.encodeToString(OfflineBook.serializer(), meta))
        if (!tmp.renameTo(target)) {
            target.delete()
            tmp.renameTo(target)
        }
    }

    fun chapters(fid: String): List<Chapter> =
        runCatching {
            json.decodeFromString<List<Chapter>>(chaptersFile(fid).readText())
        }.getOrDefault(emptyList())

    fun saveChapters(fid: String, chapters: List<Chapter>) {
        dir(fid).mkdirs()
        chaptersFile(fid).writeText(
            json.encodeToString(
                kotlinx.serialization.builtins.ListSerializer(Chapter.serializer()),
                chapters,
            ),
        )
    }

    /** Update playback position / finished flag in place (T-0343 uses this). */
    fun updateProgress(fid: String, positionS: Double, finished: Boolean) {
        val m = meta(fid) ?: return
        // Listening to it again un-hides it (SKADI-T-0647): actually resuming a
        // book is a clearer statement of interest than any button, and without
        // this a book dismissed once could never come back to Home.
        saveMeta(m.copy(positionS = positionS, finished = finished, hiddenFromHome = false))
    }

    /** Remember the speed this book is played at (SKADI-T-0659). */
    fun updateSpeed(fid: String, speed: Float) {
        val m = meta(fid) ?: return
        if (m.speed == speed) return
        saveMeta(m.copy(speed = speed))
    }

    /**
     * Drop the book off Home without touching the audio or the position
     * (SKADI-T-0647). Playing it again clears the flag — asking to resume is a
     * clearer signal of interest than any button could be.
     */
    fun setHiddenFromHome(fid: String, hidden: Boolean) {
        val m = meta(fid) ?: return
        if (m.hiddenFromHome == hidden) return
        saveMeta(m.copy(hiddenFromHome = hidden))
    }

    fun delete(fid: String) {
        dir(fid).deleteRecursively()
    }
}
