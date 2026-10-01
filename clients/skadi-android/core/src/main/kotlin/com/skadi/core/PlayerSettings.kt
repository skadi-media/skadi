package com.skadi.core

import android.content.Context

/**
 * Listener preferences for the players (SKADI-T-0658 onwards).
 *
 * SharedPreferences rather than the app's DataStore: [com.skadi.app.PlaybackService]
 * reads these on the main thread at the moment it needs them (a sleep-timer
 * tick, a resume), and a synchronous read is the honest shape for that.
 */
class PlayerSettings(context: Context) {
    private val prefs = context.applicationContext
        .getSharedPreferences("skadi.player", Context.MODE_PRIVATE)

    /** Shaking the phone near the end restarts a minutes sleep timer. */
    var shakeToExtend: Boolean
        get() = prefs.getBoolean(SHAKE, true)
        set(v) = prefs.edit().putBoolean(SHAKE, v).apply()

    /**
     * Speed for a book that has none of its own yet (SKADI-T-0659). A book
     * keeps its own speed once the listener changes it.
     */
    var defaultSpeed: Float
        get() = prefs.getFloat(DEFAULT_SPEED, 1f)
        set(v) = prefs.edit().putFloat(DEFAULT_SPEED, v).apply()

    /** Skip back length in seconds (SKADI-T-0661); one of [SKIP_CHOICES]. */
    var skipBackS: Int
        get() = prefs.getInt(SKIP_BACK, 30)
        set(v) = prefs.edit().putInt(SKIP_BACK, v).apply()

    /** Skip forward length in seconds (SKADI-T-0661). */
    var skipForwardS: Int
        get() = prefs.getInt(SKIP_FORWARD, 30)
        set(v) = prefs.edit().putInt(SKIP_FORWARD, v).apply()

    /** Rewind a little when playback resumes after a pause (SKADI-T-0661). */
    var rewindOnResume: Boolean
        get() = prefs.getBoolean(REWIND, true)
        set(v) = prefs.edit().putBoolean(REWIND, v).apply()

    /** The speed to open [book] at. */
    fun speedFor(book: OfflineBook?): Float = book?.speed ?: defaultSpeed

    companion object {
        val SKIP_CHOICES = listOf(10, 15, 30, 45, 60)

        /**
         * How far to rewind when playback resumes after [pausedMs] paused:
         * the longer the break, the more context the listener has lost. A
         * pause under 3 s is a hiccup (a seek's buffering, a notification
         * sound) and costs nothing.
         */
        fun rewindOnResumeMs(pausedMs: Long): Long = when {
            pausedMs < 3_000L -> 0L
            pausedMs < 60_000L -> 2_000L
            pausedMs < 3_600_000L -> 5_000L
            pausedMs < 86_400_000L -> 10_000L
            else -> 30_000L
        }

        private const val SHAKE = "sleep_shake_to_extend"
        private const val DEFAULT_SPEED = "default_speed"
        private const val SKIP_BACK = "skip_back_s"
        private const val SKIP_FORWARD = "skip_forward_s"
        private const val REWIND = "rewind_on_resume"
    }
}
