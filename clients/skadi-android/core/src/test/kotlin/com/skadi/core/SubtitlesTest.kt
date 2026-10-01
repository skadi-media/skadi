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
}
