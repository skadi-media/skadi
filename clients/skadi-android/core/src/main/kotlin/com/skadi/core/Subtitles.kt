package com.skadi.core

import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json
import okhttp3.OkHttpClient
import okhttp3.Request

/**
 * A subtitle file beside a video on the server (SKADI-T-0663), as
 * `GET …/subtitles` lists it.
 */
@Serializable
data class SubtitleTrack(
    val index: Int,
    val language: String? = null,
    val label: String,
    val forced: Boolean = false,
    val sdh: Boolean = false,
    /** `srt`, `vtt`, `ass` or `ssa`. */
    val format: String,
)

/**
 * The subtitle routes sit beside the video route — `…/video` becomes
 * `…/subtitles` — and take the same `?apikey=`, so a video URL is all the
 * player needs to find them.
 */
object Subtitles {
    private val json = Json { ignoreUnknownKeys = true }

    /** `…/video?apikey=k` → `…/subtitles?apikey=k`. */
    fun listUrl(videoUrl: String): String = swap(videoUrl, "/subtitles")

    /** `…/video?apikey=k` → `…/subtitles/{index}?apikey=k`. */
    fun fileUrl(videoUrl: String, index: Int): String = swap(videoUrl, "/subtitles/$index")

    private fun swap(videoUrl: String, tail: String): String {
        val q = videoUrl.indexOf('?')
        val path = if (q < 0) videoUrl else videoUrl.substring(0, q)
        val query = if (q < 0) "" else videoUrl.substring(q)
        return path.removeSuffix("/video") + tail + query
    }

    /** The video's subtitle files; empty when there are none or the server is older. */
    suspend fun fetch(videoUrl: String, client: OkHttpClient = OkHttpClient()): List<SubtitleTrack> =
        withContext(Dispatchers.IO) {
            runCatching {
                client.newCall(Request.Builder().url(listUrl(videoUrl)).build()).execute().use { resp ->
                    if (!resp.isSuccessful) emptyList()
                    else json.decodeFromString<List<SubtitleTrack>>(resp.body!!.string())
                }
            }.getOrDefault(emptyList())
        }
}

/** Where an episode's intro and credits are, in seconds (SKADI-T-0666). */
@Serializable
data class SkipMarkers(
    @kotlinx.serialization.SerialName("intro_start") val introStart: Double? = null,
    @kotlinx.serialization.SerialName("intro_end") val introEnd: Double? = null,
    @kotlinx.serialization.SerialName("credits_start") val creditsStart: Double? = null,
) {
    /** Inside the intro, with more than a second of it left to skip. */
    fun inIntro(positionS: Double): Boolean =
        introStart != null && introEnd != null && positionS >= introStart && positionS < introEnd - 1.0

    fun inCredits(positionS: Double): Boolean = creditsStart != null && positionS >= creditsStart
}

/** The markers route sits beside an episode's video route, like subtitles. */
object Markers {
    private val json = Json { ignoreUnknownKeys = true }

    /** `…/episodes/{e}/video?apikey=k` → `…/episodes/{e}/markers?apikey=k`; null for a movie. */
    fun url(videoUrl: String): String? {
        val q = videoUrl.indexOf('?')
        val path = if (q < 0) videoUrl else videoUrl.substring(0, q)
        if (!path.contains("/episodes/") || !path.endsWith("/video")) return null
        return path.removeSuffix("/video") + "/markers" + (if (q < 0) "" else videoUrl.substring(q))
    }

    suspend fun fetch(videoUrl: String, client: OkHttpClient = OkHttpClient()): SkipMarkers {
        val u = url(videoUrl) ?: return SkipMarkers()
        return withContext(Dispatchers.IO) {
            runCatching {
                client.newCall(Request.Builder().url(u).build()).execute().use { resp ->
                    if (!resp.isSuccessful) SkipMarkers()
                    else json.decodeFromString<SkipMarkers>(resp.body!!.string())
                }
            }.getOrDefault(SkipMarkers())
        }
    }
}
