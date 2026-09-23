package com.skadi.core

import android.content.Context
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json

/**
 * One thing someone has watched, or started to (SKADI-T-0578).
 *
 * Enough to draw a "continue watching" card without asking the server: title,
 * art, where they were. The playback URL is deliberately **not** here — it
 * carries `?apikey=` and is rebuilt from the ids when the card is tapped.
 */
@Serializable
data class WatchRecord(
    val key: String,
    /** `movie` or `episode`. */
    val kind: String,
    /** Movie id, or series id. */
    @SerialName("parent_id") val parentId: String,
    /** Edition id, or episode id. */
    @SerialName("item_id") val itemId: String,
    val title: String,
    val subtitle: String? = null,
    @SerialName("poster_url") val posterUrl: String? = null,
    /** Episodes only, so "up next" can find the one after this. */
    val season: Int? = null,
    val number: Int? = null,
    @SerialName("position_ms") val positionMs: Long = 0,
    @SerialName("duration_ms") val durationMs: Long = 0,
    /** Reached the end. Kept rather than deleted: the next episode is the
     *  most useful thing a home screen can offer. */
    val finished: Boolean = false,
    @SerialName("updated_at") val updatedAt: Long = 0,
) {
    val isMovie get() = kind == "movie"

    /** Position to resume at, or `null` to start from the beginning — see
     *  [VideoProgress.resumeAt] for the two refusals. */
    val resumeAtMs: Long?
        get() {
            if (finished) return null
            if (positionMs < VideoProgress.MIN_RESUME_MS) return null
            if (durationMs > 0 && positionMs > durationMs - VideoProgress.END_SLACK_MS) return null
            return positionMs
        }

    /** 0..1, or `null` when the duration is not known yet. */
    val fraction: Float?
        get() = if (durationMs > 0) (positionMs.toFloat() / durationMs).coerceIn(0f, 1f) else null
}

/**
 * Where you were in a film or episode (SKADI-T-0585), and what you have been
 * watching (SKADI-T-0578).
 *
 * Video playback always restarted from zero: nothing recorded a position, so
 * backing out of a two-hour film and returning meant scrubbing to find your
 * place. Audiobooks have had this since SKADI-T-0343 (see [OfflineStore]); video
 * simply never got it.
 *
 * Kept separate from `OfflineStore` on purpose. That store describes files the
 * device has **downloaded**, and video here is *streamed* — there is no local
 * file to hang the position off.
 *
 * Keyed by the item's **stable ids**, never by the playback URL: that URL carries
 * `?apikey=`, so it changes whenever the token is rotated and would silently
 * orphan every saved position. It would also write the API token into a
 * preferences file, which is not somewhere a credential belongs.
 *
 * One JSON [WatchRecord] per key. The first version stored a bare position and
 * duration, which is enough to resume but not enough to *show* — a home screen
 * needs the title and the poster without a round trip per card.
 */
class VideoProgress(context: Context) {
    private val prefs =
        context.getSharedPreferences("skadi_video_progress", Context.MODE_PRIVATE)
    private val json = Json { ignoreUnknownKeys = true; encodeDefaults = true }

    /** Stable key for a movie edition. */
    fun movieKey(movieId: String, editionId: String) = "movie:$movieId:$editionId"

    /** Stable key for an episode. */
    fun episodeKey(seriesId: String, episodeId: String) = "episode:$seriesId:$episodeId"

    fun get(key: String): WatchRecord? =
        prefs.getString(recKey(key), null)?.let { s ->
            runCatching { json.decodeFromString<WatchRecord>(s) }.getOrNull()
        }

    /**
     * Note that playback of this item is starting. Carries the display
     * metadata; the position, if any, is kept from the previous record so
     * opening a film does not lose your place in it.
     */
    fun start(record: WatchRecord) {
        val prev = get(record.key)
        put(
            record.copy(
                positionMs = prev?.positionMs ?: 0,
                durationMs = prev?.durationMs ?: 0,
                finished = false,
                updatedAt = System.currentTimeMillis(),
            ),
        )
    }

    /**
     * Position to resume at, in milliseconds, or `null` to start from the
     * beginning.
     *
     * Two deliberate refusals:
     * - **Near the start** (< 15s) resumes at 0. Restoring a 4-second position
     *   is worse than useless — it looks broken while doing nothing.
     * - **Near the end** (last 60s) also resumes at 0. Someone returning to a
     *   film they finished wants to watch it, not to land on the credits with
     *   no obvious way back.
     */
    fun resumeAt(key: String): Long? {
        get(key)?.let { return it.resumeAtMs }
        // Records written before SKADI-T-0578 were a bare position + duration.
        val pos = prefs.getLong("$key:pos", 0L)
        val dur = prefs.getLong("$key:dur", 0L)
        if (pos < MIN_RESUME_MS) return null
        if (dur > 0 && pos > dur - END_SLACK_MS) return null
        return pos
    }

    /**
     * Record the current position. `durationMs` may be 0 while the player is
     * still preparing, in which case the previous duration is kept rather than
     * overwritten with a zero that would defeat the end-of-file check above.
     */
    fun save(key: String, positionMs: Long, durationMs: Long) {
        val prev = get(key) ?: return legacySave(key, positionMs, durationMs)
        put(
            prev.copy(
                positionMs = positionMs,
                durationMs = if (durationMs > 0) durationMs else prev.durationMs,
                finished = false,
                updatedAt = System.currentTimeMillis(),
            ),
        )
    }

    /** Playback reached the end: the item is done, and the record stays so
     *  the home screen can offer what comes after it. */
    fun markFinished(key: String) {
        val prev = get(key)
        if (prev == null) {
            prefs.edit().remove("$key:pos").remove("$key:dur").apply()
            return
        }
        put(prev.copy(positionMs = 0, finished = true, updatedAt = System.currentTimeMillis()))
    }

    /** Drop the record entirely — "remove from continue watching". */
    fun forget(key: String) {
        prefs.edit().remove(recKey(key)).remove("$key:pos").remove("$key:dur").apply()
    }

    /** Everything with a record, most recently touched first. */
    fun recent(): List<WatchRecord> =
        prefs.all.keys
            .filter { it.startsWith(REC_PREFIX) }
            .mapNotNull { get(it.removePrefix(REC_PREFIX)) }
            .sortedByDescending { it.updatedAt }

    private fun put(record: WatchRecord) {
        prefs.edit().putString(recKey(record.key), json.encodeToString(WatchRecord.serializer(), record)).apply()
    }

    private fun legacySave(key: String, positionMs: Long, durationMs: Long) {
        prefs.edit().apply {
            putLong("$key:pos", positionMs)
            if (durationMs > 0) putLong("$key:dur", durationMs)
            apply()
        }
    }

    private fun recKey(key: String) = REC_PREFIX + key

    companion object {
        private const val REC_PREFIX = "rec:"

        /** Below this, resuming is noise rather than help. */
        const val MIN_RESUME_MS = 15_000L

        /** Within this of the end, treat it as watched. */
        const val END_SLACK_MS = 60_000L
    }
}
