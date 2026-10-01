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
 * **Not yet surname-aware.** Ordering by surname is SKADI-T-0648 F1 and is
 * blocked on a decision: Audnexus exposes no sort name (checked 2026-09-30 —
 * `/authors/{asin}` returns only asin, description, genres, image, name, region,
 * similar), so a surname has to be derived or stored. When that lands it belongs
 * in [key] alone, and [letter] will follow for free.
 */
object NameSorting {
    /**
     * Sort key for a person's name. Case- and whitespace-insensitive, and
     * deliberately *not* article-stripped.
     */
    fun key(name: String): String = name.trim().lowercase()

    /**
     * The A-Z rail letter for a name. Derived from [key] so the rail can never
     * disagree with the order; anything that is not A-Z buckets to '#'.
     */
    fun letter(name: String): Char {
        val c = key(name).firstOrNull { it.isLetterOrDigit() }?.uppercaseChar() ?: '#'
        return if (c in 'A'..'Z') c else '#'
    }
}
