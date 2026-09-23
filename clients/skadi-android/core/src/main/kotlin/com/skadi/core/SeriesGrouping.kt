package com.skadi.core

/** A series and the books in it, ordered by series position (parallels the
 *  web UI's group_books_by_series). */
data class SeriesGroup(
    val seriesId: String,
    val name: String,
    val books: List<Book>,
)

/** Numeric sort key for a series position string ("1", "1.5", "0.5"); missing
 *  or unparseable positions sort last. */
private fun seriesPosKey(b: Book): Double =
    b.series?.position?.trim()?.toDoubleOrNull() ?: Double.MAX_VALUE

/**
 * Split books into **series groups** (sorted by name; books within by position
 * then title) and **standalone** books (no series, input order preserved).
 * Pure, so it's unit-testable.
 */
fun groupBooksBySeries(books: List<Book>): Pair<List<SeriesGroup>, List<Book>> {
    val groups = LinkedHashMap<String, MutableList<Book>>()
    val names = HashMap<String, String>()
    val standalone = mutableListOf<Book>()
    for (b in books) {
        val s = b.series
        if (s == null || s.name.isBlank()) {
            standalone.add(b)
        } else {
            groups.getOrPut(s.seriesId.ifBlank { s.name }) { mutableListOf() }.add(b)
            names[s.seriesId.ifBlank { s.name }] = s.name
        }
    }
    val sortedGroups = groups.entries
        .map { (id, bs) ->
            SeriesGroup(
                seriesId = id,
                name = names[id] ?: id,
                books = bs.sortedWith(
                    compareBy({ seriesPosKey(it) }, { it.title.lowercase() }),
                ),
            )
        }
        .sortedBy { it.name.lowercase() }
    return sortedGroups to standalone
}
