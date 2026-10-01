package com.skadi.core

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import java.nio.file.Files

/** SKADI-T-0662. */
class BookmarksTest {
    private val root = Files.createTempDirectory("bm").toFile()
    private fun marks() = Bookmarks(root, "f")

    @Test
    fun `none until one is added`() {
        assertTrue(marks().list().isEmpty())
    }

    @Test
    fun `listed in book order, whatever order they were added`() {
        marks().add(300.0, "later")
        marks().add(10.0)
        assertEquals(listOf(10.0, 300.0), marks().list().map { it.positionS })
    }

    @Test
    fun `notes are edited and bookmarks deleted`() {
        val b = marks().add(42.0, "  first  ")
        assertEquals("trimmed", "first", marks().list().single().note)
        marks().updateNote(b.id, "changed")
        assertEquals("changed", marks().list().single().note)
        marks().delete(b.id)
        assertTrue(marks().list().isEmpty())
    }

    @Test
    fun `they survive a new store instance, as after an app update`() {
        marks().add(5.0, "kept")
        assertEquals("kept", Bookmarks(root, "f").list().single().note)
    }
}
