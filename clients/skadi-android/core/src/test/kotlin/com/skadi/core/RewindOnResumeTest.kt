package com.skadi.core

import org.junit.Assert.assertEquals
import org.junit.Test

/** SKADI-T-0661: the longer the pause, the further back playback resumes. */
class RewindOnResumeTest {
    private fun r(ms: Long) = PlayerSettings.rewindOnResumeMs(ms)

    @Test
    fun `a hiccup costs nothing`() {
        assertEquals(0L, r(0))
        assertEquals(0L, r(2_999))
    }

    @Test
    fun `longer breaks rewind further`() {
        assertEquals(2_000L, r(30_000))
        assertEquals(5_000L, r(10 * 60_000L))
        assertEquals(10_000L, r(3_600_000L))
        assertEquals(30_000L, r(86_400_000L))
        assertEquals(30_000L, r(7 * 86_400_000L))
    }
}
