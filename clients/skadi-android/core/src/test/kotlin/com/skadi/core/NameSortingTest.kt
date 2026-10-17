package com.skadi.core

import org.junit.Assert.assertEquals
import org.junit.Test

/**
 * Names order by the surname-first key the server emits (SKADI-T-0737), and
 * the rail letter comes from that key (SKADI-T-0648 F2). The name vector
 * itself is tested once, on the server, in `skadi_core::sorting`.
 */
class NameSortingTest {
    @Test
    fun `the server key wins over the name`() {
        assertEquals("king stephen", NameSorting.key("king stephen", "Stephen King"))
        assertEquals('K', NameSorting.letterOfKey("king stephen"))
        // "An Na" keeps its "An": the key files it under N.
        assertEquals('N', NameSorting.letterOfKey("na an"))
    }

    @Test
    fun `without a key the raw name orders`() {
        assertEquals("stephen king", NameSorting.key(null, "  Stephen   King "))
        assertEquals("dune", NameSorting.key("", "Dune"))
    }

    @Test
    fun `a book's credit reads its own key, index for index`() {
        val b = Book(
            id = "b",
            title = "Good Omens",
            authors = listOf("Terry Pratchett", "Neil Gaiman"),
            sortAuthors = listOf("pratchett terry", "gaiman neil"),
        )
        assertEquals("pratchett terry", b.authorKey(0))
        assertEquals("gaiman neil", b.authorKey(1))
        // A key list shorter than the credits falls back for the rest.
        assertEquals("neil gaiman", b.copy(sortAuthors = listOf("pratchett terry")).authorKey(1))
    }

    @Test
    fun `keys that do not start with a latin letter bucket together`() {
        assertEquals('#', NameSorting.letterOfKey("йозеф к"))
        assertEquals('#', NameSorting.letterOfKey("1984"))
        assertEquals('#', NameSorting.letterOfKey(""))
        assertEquals('H', NameSorting.letterOfKey("hooks bell"))
    }
}
