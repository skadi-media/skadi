package com.skadi.core

/**
 * Ordering for **titles** (SKADI-T-0648 F3): the one rule for how a title
 * sorts and which A-Z rail letter it files under, used by every title surface
 * — the movie grid, the TV list, the book wall and series mode. The web has
 * the same rule (`skadi-web` `sorting.rs`); both test the same vector, so the
 * same library reads in the same order on the phone and in the browser.
 *
 * One leading article ("the ", "a ", "an ") is dropped, so *The Hobbit* files
 * under H — what every library catalogue does. A person's name does not go
 * through this: "An Na" is a name, and "An" is part of it ([NameSorting]).
 *
 * [letter] is defined from [key], and [railed] hands back the sorted list with
 * its letters, so a surface cannot sort by one rule and label by another —
 * which is how the authors list went wrong (SKADI-T-0648 F2). Lives in `core`
 * so it can be unit tested; the app module has no test source set.
 */
object TitleSorting {
    private val articles = listOf("the ", "a ", "an ")

    /** Sort key for a title: trimmed, lowercase, one leading article dropped. */
    fun key(title: String): String {
        val t = title.trim().lowercase()
        return articles.firstOrNull { t.startsWith(it) }?.let { t.removePrefix(it) } ?: t
    }

    /**
     * The A-Z rail letter a title files under, derived from its [key]. A key
     * that does not start with a latin letter (digits, symbols, other scripts)
     * files under '#', one bucket rather than many.
     */
    fun letter(title: String): Char = letterOfKey(key(title))

    private fun letterOfKey(k: String): Char {
        val c = k.firstOrNull()?.uppercaseChar() ?: '#'
        return if (c in 'A'..'Z') c else '#'
    }

    /**
     * [items] in title order, each paired with its rail letter, index for
     * index. The letters come from the same key the order does, so the rail
     * can never disagree with the list.
     */
    fun <T> railed(items: List<T>, title: (T) -> String): Railed<T> {
        val sorted = items.sortedBy { key(title(it)) }
        return Railed(sorted, sorted.map { letterOfKey(key(title(it))) })
    }
}

/** A list in title order with the rail letter of each item, for `AlphabetRail`. */
data class Railed<T>(val items: List<T>, val letters: List<Char>)
