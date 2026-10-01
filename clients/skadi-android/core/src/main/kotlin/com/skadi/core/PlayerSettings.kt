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

    /** The speed to open [book] at. */
    fun speedFor(book: OfflineBook?): Float = book?.speed ?: defaultSpeed

    private companion object {
        const val SHAKE = "sleep_shake_to_extend"
        const val DEFAULT_SPEED = "default_speed"
    }
}
