package com.skadi.core

/**
 * Ordering for **people's names**, kept separate from titles (SKADI-T-0648 F2):
 * "An Na" is a name, and "An" is not an article in it.
 *
 * Names go **surname first** (SKADI-T-0648 F1): "Stephen King" orders as "king
 * stephen" and files under K. The server derives the key (`sort_name` on an
 * author, `sort_authors` on a book, SKADI-T-0737) from the one copy of the rule
 * in `skadi_core::sorting`; the app holds none. A record from an older server
 * has no key and orders by the raw name, trimmed and lowercased.
 *
 * [letterOfKey] reads the key, so the rail can never disagree with the order.
 */
object NameSorting {
    /** The key a name sorts by: the server's [sortKey], else [name] trimmed,
     *  lowercased and with its spacing collapsed. */
    fun key(sortKey: String?, name: String): String =
        if (sortKey != null && (sortKey.isNotEmpty() || name.isBlank())) {
            sortKey
        } else {
            name.trim().lowercase().replace(Regex("\\s+"), " ")
        }

    /** The A-Z rail letter for a name key; anything that is not A-Z buckets to '#'. */
    fun letterOfKey(k: String): Char {
        val c = k.firstOrNull { it.isLetterOrDigit() }?.uppercaseChar() ?: '#'
        return if (c in 'A'..'Z') c else '#'
    }
}

/** The surname-first key of the credit at [index] of a book's authors. */
fun Book.authorKey(index: Int): String = NameSorting.key(sortAuthors.getOrNull(index), authors[index])
