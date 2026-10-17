package com.skadi.core

/** One row of the authors view (SKADI-T-0655). */
data class AuthorGroup(
    /** Stable row key: `id:<authorId>` for a linked author, else `name:<nameKey>`.
     *  Distinct per row, so two people with one name are two LazyColumn items. */
    val key: String,
    /** The display name, as the first book credits it. */
    val name: String,
    val authorId: String?,
    /** Every book credited to the author, in title order. */
    val books: List<Book>,
    /** Set only when another row shows the same name: the first book's title,
     *  so the two rows can be told apart. */
    val hint: String?,
)

/**
 * Group the library by author for the authors view (the Android parallel of
 * the web's `group_books_by_author`).
 *
 * Every credit counts, so a co-author's row lists the book too. A book's
 * primary credit (its first non-blank author) groups by the linked record
 * (`authorId`, SKADI-T-0653) when there is one; every other credit, and any
 * book the server has not linked, groups by name key ([nameKey]). Two
 * different people whose names key alike therefore stay two rows when both
 * are linked. An unlinked credit joins a linked row only when exactly one row
 * carries its name — the rule the server links by — else it keeps its own
 * row. Rows are in surname order ([NameSorting.key]). Pure.
 */
fun groupBooksByAuthor(books: List<Book>): List<AuthorGroup> {
    data class Credit(val name: String, val book: Book)

    val linked = LinkedHashMap<String, MutableList<Credit>>()
    val linkedName = HashMap<String, String>()
    val loose = mutableListOf<Credit>()
    for (b in books) {
        val primary = b.authors.firstOrNull { it.isNotBlank() }
        for (name in b.authors) {
            if (name.isBlank()) continue
            val id = b.authorId?.takeIf { it.isNotBlank() }
            if (name == primary && id != null) {
                val key = "id:$id"
                linked.getOrPut(key) { linkedName[key] = name; mutableListOf() }.add(Credit(name, b))
            } else {
                loose.add(Credit(name, b))
            }
        }
    }
    // Linked rows by name key: a loose credit folds into the one row of its
    // name, never into one of two.
    val linkedByName = linked.keys.groupBy { nameKey(linkedName.getValue(it)) }
    val rows = LinkedHashMap<String, MutableList<Credit>>()
    val rowName = HashMap<String, String>()
    for ((key, credits) in linked) {
        rows[key] = credits
        rowName[key] = linkedName.getValue(key)
    }
    for (c in loose) {
        val nk = nameKey(c.name)
        val key = linkedByName[nk]?.singleOrNull() ?: "name:$nk"
        rows.getOrPut(key) { rowName[key] = c.name; mutableListOf() }.add(c)
    }
    val groups = rows.map { (key, credits) ->
        AuthorGroup(
            key = key,
            name = rowName.getValue(key),
            authorId = key.removePrefix("id:").takeIf { key.startsWith("id:") },
            books = credits.map { it.book }.distinctBy { it.id }.sortedBy { TitleSorting.key(it.title) },
            hint = null,
        )
    }
    val shared = groups.groupingBy { nameKey(it.name) }.eachCount()
    return groups
        .map { g ->
            if ((shared[nameKey(g.name)] ?: 0) > 1) {
                g.copy(hint = g.books.firstOrNull()?.let { "e.g. ${it.title}" })
            } else {
                g
            }
        }
        .sortedBy { NameSorting.key(it.name) }
}
