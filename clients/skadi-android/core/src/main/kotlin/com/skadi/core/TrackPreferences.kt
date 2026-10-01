package com.skadi.core

import android.content.Context
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json

/** When subtitles show (SKADI-T-0664). */
@Serializable
enum class SubtitleMode {
    /** Never, not even forced ones. */
    Off,

    /** Only forced subtitles: the foreign-language lines of a film in your language. */
    Forced,

    /** Always, in [TrackPrefs.subtitleLanguage] (or the audio language). */
    On,
}

/** Which audio and subtitle tracks to start a video with (SKADI-T-0664). */
@Serializable
data class TrackPrefs(
    /** ISO 639 code (`"en"`); null leaves it to the file's default track. */
    val audio: String? = null,
    val subtitles: SubtitleMode = SubtitleMode.Forced,
    /** For [SubtitleMode.On]; null means the audio language. */
    val subtitleLanguage: String? = null,
)

/**
 * The viewer's track preferences: one global set, and one per series — a
 * choice made while watching a series is about that series (an anime watched
 * in Japanese with subtitles, a sitcom in English without), so it does not
 * change the rest. Device-local, like resume positions.
 */
class TrackPreferences(context: Context) {
    private val prefs = context.applicationContext
        .getSharedPreferences("skadi.tracks", Context.MODE_PRIVATE)
    private val json = Json { ignoreUnknownKeys = true; encodeDefaults = true }

    var global: TrackPrefs
        get() = read(GLOBAL) ?: TrackPrefs()
        set(v) = write(GLOBAL, v)

    fun series(seriesId: String): TrackPrefs? = read("series:$seriesId")

    fun setSeries(seriesId: String, v: TrackPrefs) = write("series:$seriesId", v)

    /** What to start the video under [progressKey] with: its series' choice, else the global one. */
    fun effective(progressKey: String): TrackPrefs =
        seriesIdOf(progressKey)?.let(::series) ?: global

    private fun read(key: String): TrackPrefs? =
        prefs.getString(key, null)?.let { runCatching { json.decodeFromString<TrackPrefs>(it) }.getOrNull() }

    private fun write(key: String, v: TrackPrefs) =
        prefs.edit().putString(key, json.encodeToString(TrackPrefs.serializer(), v)).apply()

    companion object {
        private const val GLOBAL = "global"

        /** The series of an episode resume key (`episode:{series}:{episode}`); null for a movie. */
        fun seriesIdOf(progressKey: String): String? =
            progressKey.split(':').takeIf { it.size == 3 && it[0] == "episode" }?.get(1)

        /** Languages offered in the picker: code to name. */
        val LANGUAGES = listOf(
            "en" to "English", "es" to "Spanish", "fr" to "French", "de" to "German",
            "it" to "Italian", "pt" to "Portuguese", "nl" to "Dutch", "sv" to "Swedish",
            "no" to "Norwegian", "da" to "Danish", "fi" to "Finnish", "pl" to "Polish",
            "ru" to "Russian", "ja" to "Japanese", "ko" to "Korean", "zh" to "Chinese",
        )
    }
}
