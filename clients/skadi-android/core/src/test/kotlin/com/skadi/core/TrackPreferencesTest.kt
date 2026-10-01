package com.skadi.core

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

/** SKADI-T-0664. */
class TrackPreferencesTest {
    @Test
    fun `the series of an episode key, none for a movie`() {
        assertEquals("s1", TrackPreferences.seriesIdOf("episode:s1:e9"))
        assertNull(TrackPreferences.seriesIdOf("movie:m1:ed1"))
        assertNull(TrackPreferences.seriesIdOf("nonsense"))
    }

    @Test
    fun `defaults show forced subtitles and leave audio to the file`() {
        val p = TrackPrefs()
        assertNull(p.audio)
        assertEquals(SubtitleMode.Forced, p.subtitles)
    }
}
