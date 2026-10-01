package com.skadi.core

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test
import java.nio.file.Files

/** SKADI-T-0659: a book keeps its own speed; progress writes do not lose it. */
class OfflineSpeedTest {
    private fun store(): OfflineStore {
        val root = Files.createTempDirectory("offline").toFile()
        return OfflineStore(root).also {
            it.saveMeta(OfflineBook(bookId = "b", fileId = "f", title = "T"))
        }
    }

    @Test
    fun `a new book has no speed of its own`() {
        assertNull(store().meta("f")!!.speed)
    }

    @Test
    fun `the speed survives progress updates`() {
        val s = store()
        s.updateSpeed("f", 1.5f)
        s.updateProgress("f", 120.0, finished = false)
        assertEquals(1.5f, s.meta("f")!!.speed!!, 0f)
        assertEquals(120.0, s.meta("f")!!.positionS, 0.0)
    }

    @Test
    fun `meta written before speeds existed still loads`() {
        val root = Files.createTempDirectory("offline").toFile()
        java.io.File(root, "books/f").mkdirs()
        java.io.File(root, "books/f/meta.json")
            .writeText("""{"book_id":"b","file_id":"f","title":"T","position_s":3.0}""")
        val m = OfflineStore(root).meta("f")!!
        assertNull(m.speed)
        assertEquals(3.0, m.positionS, 0.0)
    }
}
