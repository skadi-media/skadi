package com.skadi.core

/**
 * Ordering for **titles** (SKADI-T-0648, SKADI-T-0737). The server emits the
 * sort key of each list record (`sort_title`), derived once by
 * `skadi_core::sorting`; the app orders by that key and holds no copy of the
 * rule. The web does the same, so the same library reads in the same order on
 * the phone and in the browser.
 *
 * A record from an older server has no key. It still orders: [key] falls back
 * to the raw title, trimmed and lowercased.
 *
 * The rail letter comes from the key ([letterOfKey]), and [railed] hands back
 * the sorted list with its letters, so a surface cannot sort by one rule and
 * label by another — which is how the authors list went wrong (SKADI-T-0648 F2).
 */
object TitleSorting {
    /** The key a record sorts by: the server's [sortKey], else [title] trimmed
     *  and lowercased. An empty key is trusted only for a blank title. */
    fun key(sortKey: String?, title: String): String =
        if (sortKey != null && (sortKey.isNotEmpty() || title.isBlank())) sortKey else title.trim().lowercase()

    /**
     * The A-Z rail letter a key files under. A key that does not start with a
     * latin letter (digits, symbols, other scripts) files under '#', one bucket
     * rather than many.
     */
    fun letterOfKey(k: String): Char {
        val c = k.firstOrNull()?.uppercaseChar() ?: '#'
        return if (c in 'A'..'Z') c else '#'
    }

    /**
     * [items] in key order, each paired with its rail letter, index for index.
     * The letters come from the same key the order does, so the rail can never
     * disagree with the list.
     */
    fun <T> railed(items: List<T>, key: (T) -> String): Railed<T> {
        val keyed = items.map { it to key(it) }.sortedBy { it.second }
        return Railed(keyed.map { it.first }, keyed.map { letterOfKey(it.second) })
    }
}

/** A list in title order with the rail letter of each item, for `AlphabetRail`. */
data class Railed<T>(val items: List<T>, val letters: List<Char>)

/** The title key of a movie (SKADI-T-0737). */
val Movie.sortKey: String get() = TitleSorting.key(sort_title, title)

/** The key of a movie's franchise, for a collapsed franchise tile. */
val Movie.collectionSortKey: String
    get() = TitleSorting.key(sort_collection, collection?.name.orEmpty())

/** The title key of a series. */
val Series.sortKey: String get() = TitleSorting.key(sort_title, title)

/** The title key of a book. */
val Book.sortKey: String get() = TitleSorting.key(sortTitle, title)

/** The key of a book's series name; empty when it is in none. */
val Book.seriesSortKey: String get() = TitleSorting.key(sortSeries, series?.name.orEmpty())

/** The title key of a downloaded book, saved from the server's key at download. */
val OfflineBook.sortKey: String get() = TitleSorting.key(sortTitle, title)

/** The title key of a work. */
val Work.sortKey: String get() = TitleSorting.key(sortTitle, title)
