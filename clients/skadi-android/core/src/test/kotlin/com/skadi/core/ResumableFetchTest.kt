package com.skadi.core

import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Test
import java.io.File
import java.nio.file.Files
import java.util.concurrent.CancellationException

/** SKADI-T-0637: the Range fetch books and video share. */
class ResumableFetchTest {
    private val server = FakeServer()
    private val dir = Files.createTempDirectory("fetch").toFile()
    private val part = File(dir, "video.part")
    private val target = File(dir, "video")
    private val data = bytes(1_000_000)

    init {
        server.files["/v"] = data
    }

    private fun fetch(free: Long = Long.MAX_VALUE, cancelAfter: Int? = null) {
        var calls = 0
        ResumableFetch.fetch(
            client = server.client,
            url = "http://skadi.test/v",
            authHeader = "Authorization" to "Bearer k",
            part = part,
            target = target,
            onProgress = {},
            isCancelled = { cancelAfter != null && calls++ >= cancelAfter },
            freeSpace = { free },
        )
    }

    @Test
    fun `a fresh fetch writes the whole file and no part`() {
        fetch()
        assertArrayEquals(data, target.readBytes())
        assertFalse(part.exists())
        assertNull(server.requests.single().header("Range"))
        assertEquals("Bearer k", server.requests.single().header("Authorization"))
    }

    @Test
    fun `a part file resumes from its length`() {
        part.writeBytes(data.copyOfRange(0, 300_000))
        fetch()
        assertEquals("bytes=300000-", server.requests.single().header("Range"))
        assertArrayEquals(data, target.readBytes())
    }

    @Test
    fun `a dropped connection keeps the part and the next try finishes it`() {
        server.cutAfter = 400_000
        try {
            fetch()
            fail("a short body must not count as complete")
        } catch (e: IllegalStateException) {
            assertTrue(e.message!!.contains("truncated"))
        }
        assertFalse(target.exists())
        assertEquals(400_000L, part.length())
        server.cutAfter = null
        fetch()
        assertEquals("bytes=400000-", server.requests.last().header("Range"))
        assertArrayEquals(data, target.readBytes())
    }

    @Test
    fun `a server that ignores Range gets a clean restart, not stale tail bytes`() {
        part.writeBytes(ByteArray(500_000) { 7 })
        server.ignoreRange = true
        fetch()
        assertArrayEquals(data, target.readBytes())
    }

    @Test
    fun `a 206 at the wrong offset starts again from zero`() {
        part.writeBytes(data.copyOfRange(0, 300_000))
        server.wrongStart = 100
        fetch()
        assertEquals(2, server.requests.size)
        assertNull(server.requests.last().header("Range"))
        assertArrayEquals(data, target.readBytes())
    }

    @Test
    fun `416 clears the part and tries once more`() {
        part.writeBytes(data)
        fetch()
        assertEquals(2, server.requests.size)
        assertArrayEquals(data, target.readBytes())
    }

    @Test
    fun `416 twice gives up`() {
        server.force416 = 2
        try {
            fetch()
            fail("two 416s must fail")
        } catch (e: IllegalStateException) {
            assertTrue(e.message!!.contains("416"))
        }
    }

    @Test
    fun `a file that does not fit is refused before any bytes are written`() {
        try {
            fetch(free = 1_000_000L + ResumableFetch.RESERVE_BYTES - 1)
            fail("must refuse")
        } catch (e: ResumableFetch.NoSpaceException) {
            assertEquals(1_000_000L, e.needed)
        }
        assertEquals(0L, part.length())
        assertFalse(target.exists())
    }

    @Test
    fun `the space check counts only the bytes still to come`() {
        part.writeBytes(data.copyOfRange(0, 900_000))
        fetch(free = 100_000L + ResumableFetch.RESERVE_BYTES)
        assertArrayEquals(data, target.readBytes())
    }

    @Test
    fun `cancel stops the write and keeps no finished file`() {
        try {
            fetch(cancelAfter = 1)
            fail("must stop")
        } catch (_: CancellationException) {
        }
        assertFalse(target.exists())
        assertTrue(part.length() < data.size)
    }

    @Test
    fun `content range parsing`() {
        assertEquals(10L to 100L, ResumableFetch.parseContentRange("bytes 10-99/100"))
        assertEquals(10L to null, ResumableFetch.parseContentRange("bytes 10-99/*"))
        assertNull(ResumableFetch.parseContentRange(null))
        assertNull(ResumableFetch.parseContentRange("garbage"))
        assertNull(ResumableFetch.rangeHeader(0))
        assertEquals("bytes=5-", ResumableFetch.rangeHeader(5))
    }

    @Test
    fun `fits keeps the reserve and lets an unknown size through`() {
        assertTrue(ResumableFetch.fits(remaining = 100, free = 200, reserve = 100))
        assertFalse(ResumableFetch.fits(remaining = 101, free = 200, reserve = 100))
        assertTrue(ResumableFetch.fits(remaining = -1, free = 0, reserve = 100))
    }
}
