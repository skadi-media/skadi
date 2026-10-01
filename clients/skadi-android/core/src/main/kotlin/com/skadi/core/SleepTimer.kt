package com.skadi.core

/**
 * The audiobook sleep timer's state and arithmetic (SKADI-T-0657).
 *
 * The timer used to be two absolute targets: a wall-clock deadline and an
 * audio position. Both were wrong in ordinary use:
 *
 *  - The deadline kept running while paused and was never cleared, so a timer
 *    that expired during a pause stopped the *next* session seconds after play.
 *  - End of chapter stored the chapter's end at the moment it was set, so a
 *    seek to another chapter made it stop at once, or play on past the chapter
 *    actually playing.
 *
 * So the timer holds a *mode*. A minutes timer counts **listening** time — it
 * runs only while playing. End of chapter is resolved against the position
 * every time it is asked, so it always means "the chapter playing now".
 *
 * Pure: the clock is injected, and nothing here touches the player. The
 * service asks [msUntilStop] and schedules a callback for that moment, rather
 * than polling.
 */
class SleepTimer(private val now: () -> Long = System::currentTimeMillis) {
    sealed interface Mode {
        /** Stop after [totalMs] of listening; [remainingMs] as of the last pause. */
        data class Minutes(val totalMs: Long, val remainingMs: Long) : Mode
        data object EndOfChapter : Mode
    }

    var mode: Mode? = null
        private set

    /** Wall-clock start of the current playing stretch; null while paused. */
    private var runningSince: Long? = null

    fun armMinutes(minutes: Int, playing: Boolean) {
        val ms = minutes * 60_000L
        mode = Mode.Minutes(ms, ms)
        runningSince = if (playing) now() else null
    }

    fun armEndOfChapter() {
        mode = Mode.EndOfChapter
        runningSince = null
    }

    fun cancel() {
        mode = null
        runningSince = null
    }

    /** Restart a minutes timer at its full length (shake to extend). */
    fun restart(playing: Boolean) {
        val m = mode as? Mode.Minutes ?: return
        mode = Mode.Minutes(m.totalMs, m.totalMs)
        runningSince = if (playing) now() else null
    }

    /** Playback started or stopped. Only a minutes timer cares. */
    fun onPlayingChanged(playing: Boolean) {
        val m = mode as? Mode.Minutes ?: return
        if (playing) {
            if (runningSince == null) runningSince = now()
        } else {
            mode = m.copy(remainingMs = remainingMs()!!)
            runningSince = null
        }
    }

    /** Listening time left on a minutes timer; null for any other mode. */
    fun remainingMs(): Long? {
        val m = mode as? Mode.Minutes ?: return null
        val ran = runningSince?.let { now() - it } ?: 0L
        return (m.remainingMs - ran).coerceAtLeast(0L)
    }

    /**
     * Wall-clock milliseconds until playback must stop, or null when nothing
     * is armed or there is no chapter to end. [positionS] is the book
     * position and [speed] the playback speed: a chapter end 30 s of audio away
     * is 15 s away at 2×.
     */
    fun msUntilStop(positionS: Double, speed: Float, chapters: List<Chapter>): Long? =
        when (mode) {
            null -> null
            is Mode.Minutes -> remainingMs()
            Mode.EndOfChapter -> chapterEndS(chapters, positionS)?.let { end ->
                (((end - positionS) / speed.coerceAtLeast(0.1f)) * 1000).toLong().coerceAtLeast(0L)
            }
        }

    companion object {
        /**
         * End of the chapter playing at [positionS]. A position exactly on a
         * boundary belongs to the chapter that starts there.
         */
        fun chapterEndS(chapters: List<Chapter>, positionS: Double): Double? =
            chapters.lastOrNull { it.startS <= positionS }?.endS
                ?: chapters.firstOrNull()?.endS
    }
}
