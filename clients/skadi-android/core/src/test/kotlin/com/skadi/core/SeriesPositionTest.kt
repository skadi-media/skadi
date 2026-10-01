package com.skadi.core

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/** SKADI-T-0654: unpositioned series members are "Related", not counted. */
class SeriesPositionTest {
    @Test
    fun `zero and fractional positions count, null and blank do not`() {
        assertTrue(hasSeriesPosition("1"))
        assertTrue("prequels are position 0", hasSeriesPosition("0"))
        assertTrue(hasSeriesPosition("1.5"))
        assertFalse(hasSeriesPosition(null))
        assertFalse(hasSeriesPosition("   "))
    }

    @Test
    fun `numbered members come first and the related ones are split off in order`() {
        val members = listOf(
            "A Game of Thrones" to "1",
            "Dangerous Women" to null,
            "A Clash of Kings" to "2",
            "The Book of Swords" to "",
        )
        val (numbered, related) = splitRelated(members) { it.second }
        assertEquals(listOf("A Game of Thrones", "A Clash of Kings"), numbered.map { it.first })
        assertEquals(listOf("Dangerous Women", "The Book of Swords"), related.map { it.first })
    }
}
