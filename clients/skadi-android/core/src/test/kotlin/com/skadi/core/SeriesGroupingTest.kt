package com.skadi.core

import kotlinx.serialization.json.JsonPrimitive
import org.junit.Assert.assertEquals
import org.junit.Test

class SeriesGroupingTest {
    private fun book(title: String, series: String? = null, pos: String? = null) =
        Book(
            id = title,
            title = title,
            series = series?.let { SeriesLink(seriesId = it, name = it, position = pos) },
            files = listOf(BookFile(id = "f-$title", status = JsonPrimitive("Imported"))),
        )

    @Test
    fun groups_by_series_ordered_by_position_standalone_preserved() {
        val (groups, standalone) = groupBooksBySeries(
            listOf(
                book("Standalone A"),
                book("Book Two", "Expanse", "2"),
                book("Book One", "Expanse", "1"),
                book("Book Ten", "Expanse", "10"),
                book("Standalone B"),
                book("Only", "Solo", "1"),
            ),
        )
        // Series sorted by name; Expanse before Solo.
        assertEquals(listOf("Expanse", "Solo"), groups.map { it.name })
        // Numeric position order (10 after 2, not lexical).
        assertEquals(
            listOf("Book One", "Book Two", "Book Ten"),
            groups[0].books.map { it.title },
        )
        // Standalone preserves input order.
        assertEquals(listOf("Standalone A", "Standalone B"), standalone.map { it.title })
    }
}
