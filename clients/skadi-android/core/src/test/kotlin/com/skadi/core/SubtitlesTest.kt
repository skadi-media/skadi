package com.skadi.core

import org.junit.Assert.assertEquals
import org.junit.Test

/** SKADI-T-0663. */
class SubtitlesTest {
    private val video = "http://h:8090/api/v1/movies/m/editions/e/video?apikey=k"

    @Test
    fun `subtitle urls sit beside the video url and keep its key`() {
        assertEquals("http://h:8090/api/v1/movies/m/editions/e/subtitles?apikey=k", Subtitles.listUrl(video))
        assertEquals("http://h:8090/api/v1/movies/m/editions/e/subtitles/2?apikey=k", Subtitles.fileUrl(video, 2))
    }

    @Test
    fun `a url without a key works too`() {
        assertEquals(
            "http://h/api/v1/series/s/episodes/x/subtitles",
            Subtitles.listUrl("http://h/api/v1/series/s/episodes/x/video"),
        )
    }

    // --- SKADI-T-0666 ---

    @Test
    fun `markers only for episodes`() {
        assertEquals(
            "http://h/api/v1/series/s/episodes/x/markers?apikey=k",
            Markers.url("http://h/api/v1/series/s/episodes/x/video?apikey=k"),
        )
        assertEquals(null, Markers.url(video))
    }

    @Test
    fun `skip intro shows during the intro, not in its last second`() {
        val m = SkipMarkers(introStart = 90.0, introEnd = 180.0, creditsStart = 1290.0)
        assertEquals(false, m.inIntro(89.0))
        assertEquals(true, m.inIntro(90.0))
        assertEquals(false, m.inIntro(179.5))
        assertEquals(false, m.inCredits(1289.0))
        assertEquals(true, m.inCredits(1290.0))
        assertEquals(false, SkipMarkers().inIntro(100.0))
    }
}
