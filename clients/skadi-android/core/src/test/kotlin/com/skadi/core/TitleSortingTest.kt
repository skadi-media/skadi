package com.skadi.core

import org.junit.Assert.assertEquals
import org.junit.Test

/**
 * The app orders by the key the server emits (SKADI-T-0737) and the rail
 * cannot drift from it (SKADI-T-0648 F3). The title vector itself is tested
 * once, on the server, in `skadi_core::sorting`.
 */
class TitleSortingTest {
    @Test
    fun `the server key wins over the title`() {
        assertEquals("hobbit", TitleSorting.key("hobbit", "The Hobbit"))
        val m = Movie(id = "m", title = "The Matrix", sort_title = "matrix")
        assertEquals("matrix", m.sortKey)
    }

    @Test
    fun `without a key the raw title orders`() {
        // An older server: "The Hobbit" files under T.
        assertEquals("the hobbit", TitleSorting.key(null, "  The Hobbit "))
        assertEquals("the hobbit", Book(id = "b", title = "The Hobbit").sortKey)
        // An empty key is trusted only for a blank title.
        assertEquals("dune", TitleSorting.key("", "Dune"))
        assertEquals("", TitleSorting.key("", "  "))
    }

    @Test
    fun `railed orders by the key and letters from it`() {
        val items = listOf("The Hobbit" to "hobbit", "1984" to "1984", "Dune" to "dune", "Anathem" to "anathem")
        val railed = TitleSorting.railed(items) { it.second }
        assertEquals(listOf("1984", "Anathem", "Dune", "The Hobbit"), railed.items.map { it.first })
        assertEquals(listOf('#', 'A', 'D', 'H'), railed.letters)
    }

    @Test
    fun `keys that do not start with a latin letter bucket together`() {
        assertEquals('#', TitleSorting.letterOfKey("¡three amigos!"))
        assertEquals('#', TitleSorting.letterOfKey("100"))
        assertEquals('#', TitleSorting.letterOfKey(""))
        assertEquals('T', TitleSorting.letterOfKey("them!"))
    }

    @Test
    fun `a franchise tile orders by the franchise key`() {
        val m = Movie(
            id = "m",
            title = "Taken 2",
            sort_title = "taken 2",
            collection = MovieCollection(tmdb_id = 1, name = "The Taken Collection"),
            sort_collection = "taken collection",
        )
        assertEquals("taken collection", m.collectionSortKey)
        assertEquals("the taken collection", m.copy(sort_collection = null).collectionSortKey)
    }
}
