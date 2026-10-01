package com.skadi.core

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

/** SKADI-T-0657: the sleep timer counts listening time and follows the chapter playing. */
class SleepTimerTest {
    private var clock = 0L
    private fun timer() = SleepTimer { clock }

    private val chapters = listOf(
        Chapter(0, "One", 0.0, 600.0),
        Chapter(1, "Two", 600.0, 1500.0),
        Chapter(2, "Three", 1500.0, 2000.0),
    )

    @Test
    fun `a minutes timer counts down only while playing`() {
        val t = timer()
        t.armMinutes(15, playing = true)
        clock += 10 * 60_000L
        t.onPlayingChanged(false)
        assertEquals("5 min left at the pause", 5 * 60_000L, t.remainingMs())
        clock += 60 * 60_000L
        assertEquals("an hour paused costs nothing", 5 * 60_000L, t.remainingMs())
        t.onPlayingChanged(true)
        clock += 60_000L
        assertEquals(4 * 60_000L, t.remainingMs())
    }

    /** The production defect: set 15 min, pause at 10, resume an hour later. */
    @Test
    fun `a pause never makes a later session stop at once`() {
        val t = timer()
        t.armMinutes(15, playing = true)
        clock += 10 * 60_000L
        t.onPlayingChanged(false)
        clock += 60 * 60_000L
        t.onPlayingChanged(true)
        assertEquals(5 * 60_000L, t.msUntilStop(0.0, 1f, chapters))
    }

    @Test
    fun `arming while paused does not start the clock`() {
        val t = timer()
        t.armMinutes(30, playing = false)
        clock += 10 * 60_000L
        assertEquals(30 * 60_000L, t.remainingMs())
    }

    @Test
    fun `end of chapter follows the chapter playing now, after any seek`() {
        val t = timer()
        t.armEndOfChapter()
        assertEquals(100_000L, t.msUntilStop(500.0, 1f, chapters))
        // Seek into chapter two: its end, not chapter one's.
        assertEquals(900_000L, t.msUntilStop(600.0, 1f, chapters))
        // Seek back into chapter one.
        assertEquals(590_000L, t.msUntilStop(10.0, 1f, chapters))
    }

    @Test
    fun `end of chapter is wall time, so speed shortens it`() {
        val t = timer()
        t.armEndOfChapter()
        assertEquals(50_000L, t.msUntilStop(500.0, 2f, chapters))
        assertEquals(33_333L, t.msUntilStop(500.0, 3f, chapters))
    }

    @Test
    fun `end of chapter without chapters schedules nothing`() {
        val t = timer()
        t.armEndOfChapter()
        assertNull(t.msUntilStop(10.0, 1f, emptyList()))
    }

    @Test
    fun `a position on a boundary belongs to the chapter starting there`() {
        assertEquals(1500.0, SleepTimer.chapterEndS(chapters, 600.0)!!, 0.0)
        assertEquals(600.0, SleepTimer.chapterEndS(chapters, 599.9)!!, 0.0)
    }

    @Test
    fun `cancel and restart`() {
        val t = timer()
        t.armMinutes(15, playing = true)
        clock += 14 * 60_000L
        t.restart(playing = true)
        assertEquals(15 * 60_000L, t.remainingMs())
        t.cancel()
        assertNull(t.mode)
        assertNull(t.msUntilStop(0.0, 1f, chapters))
    }

    // --- SKADI-T-0658 ---

    @Test
    fun `the fade runs over the last ten seconds only`() {
        assertEquals(1f, SleepTimer.fadeVolume(60_000L), 0f)
        assertEquals(1f, SleepTimer.fadeVolume(10_000L), 0f)
        assertEquals(0.5f, SleepTimer.fadeVolume(5_000L), 0.001f)
        assertEquals(0f, SleepTimer.fadeVolume(0L), 0f)
    }

    @Test
    fun `the service wakes at each stage, not continuously`() {
        assertEquals("to the shake window", 29 * 60_000L, SleepTimer.nextTickMs(30 * 60_000L))
        assertEquals("to the fade", 50_000L, SleepTimer.nextTickMs(60_000L))
        assertEquals("through the fade", 200L, SleepTimer.nextTickMs(9_000L))
        assertEquals(30L, SleepTimer.nextTickMs(30L))
    }

    @Test
    fun `status labels`() {
        assertNull(SleepTimer.statusLabel(null, null))
        assertEquals("Sleep in 14:32", SleepTimer.statusLabel(SleepTimer.Mode.Minutes(15 * 60_000L, 0), 872_000L))
        assertEquals("Sleep in 1:00:00", SleepTimer.statusLabel(SleepTimer.Mode.Minutes(60 * 60_000L, 0), 3_600_000L))
        assertEquals("Sleep at chapter end", SleepTimer.statusLabel(SleepTimer.Mode.EndOfChapter, null))
        assertEquals("rounds up", "0:01", SleepTimer.clock(1L))
    }
}
