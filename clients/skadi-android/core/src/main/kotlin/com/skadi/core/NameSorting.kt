package com.skadi.core

/**
 * Ordering for **people's names**, kept separate from the title ordering in
 * `Browse.kt` (SKADI-T-0648 F2).
 *
 * The authors list used to sort on `name.lowercase()` while its A-Z rail took
 * letters from `sortLetter`, a *title* function that strips a leading "the ",
 * "a " or "an ". Two different rules over one list: an author such as **An Na**
 * sorted under A and was labelled N, so tapping the rail scrolled to the wrong
 * place.
 *
 * Aligning the list onto `sortLetter` would have made both answers agree and
 * both wrong — article stripping is a rule about *titles*. "An Na" is a name;
 * "An" is not an article in it. So names get their own pair, and the pair is the
 * point: [key] and [letter] must always derive from the same normalisation, and
 * [letter] is defined in terms of [key] so they cannot drift apart.
 *
 * Lives in `core` rather than beside the title functions so it can be unit
 * tested — the app module has no test source set.
 *
 * **Surname first** (SKADI-T-0648 F1). Audnexus exposes no sort name, so the
 * surname is derived: the last word, plus any particles before it ("Le Guin",
 * "van Vogt"), ignoring a generational or degree suffix ("Jr.", "III"). The key
 * is the surname followed by the rest, so "Stephen King" orders as "king
 * stephen" and files under K. A name already written "Surname, Given" is taken
 * as written. Collective credits ("Full Cast") are not people and keep their
 * order. The same rule lives in the web client (`skadi-web` `name_sort_key`);
 * both test the same vector.
 */
object NameSorting {
    private val particles = setOf(
        "le", "la", "de", "du", "da", "di", "del", "della", "des", "van", "von",
        "der", "den", "ter", "ten", "dos", "das", "st", "st.", "al", "el", "bin", "ibn",
    )
    private val suffixes = setOf(
        "jr", "jr.", "sr", "sr.", "ii", "iii", "iv", "phd", "ph.d.", "md", "m.d.",
    )
    private val collective = setOf(
        "full cast", "various", "various authors", "anonymous", "unknown author",
    )

    /**
     * Sort key for a person's name: surname, then the rest, lowercased.
     * Case- and whitespace-insensitive, and deliberately *not* article-stripped.
     */
    fun key(name: String): String {
        val n = name.trim().lowercase().replace(Regex("\\s+"), " ")
        if (n.isEmpty() || n in collective) return n
        if (", " in n) {
            val (head, tail) = n.split(", ", limit = 2)
            if (tail.trimEnd(',') !in suffixes) return "$head $tail"
        }
        val words = n.split(' ').map { it.trimEnd(',') }.toMutableList()
        while (words.size > 1 && words.last() in suffixes) words.removeAt(words.size - 1)
        if (words.size == 1) return words[0]
        var start = words.size - 1
        while (start > 1 && words[start - 1] in particles) start--
        return (words.subList(start, words.size) + words.subList(0, start)).joinToString(" ")
    }

    /**
     * The A-Z rail letter for a name. Derived from [key] so the rail can never
     * disagree with the order; anything that is not A-Z buckets to '#'.
     */
    fun letter(name: String): Char {
        val c = key(name).firstOrNull { it.isLetterOrDigit() }?.uppercaseChar() ?: '#'
        return if (c in 'A'..'Z') c else '#'
    }
}
