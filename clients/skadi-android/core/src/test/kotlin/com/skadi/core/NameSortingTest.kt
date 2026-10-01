package com.skadi.core

import org.junit.Assert.assertEquals
import org.junit.Test

/**
 * The invariant F2 broke: the rail's letter and the list's order must come from
 * the same rule (SKADI-T-0648).
 */
class NameSortingTest {
    /** The library's real awkward cases, plus the one that exposed the bug. */
    private val names = listOf(
        "An Na",                   // starts with an article-looking word
        "A. G. Riddle",            // initials
        "A.C. Cobble",             // initials, no spaces
        "Agatha Christie",
        "bell hooks",              // lowercase by the author's insistence
        "Ursula K. Le Guin",       // particle surname
        "Stephen King",
        "  Alan Lee  ",            // stray whitespace
        "Йозеф К",                 // non-ASCII first letter
        "1984",                    // digits only
    )

    @Test
    fun `every name's rail letter agrees with where the sort key puts it`() {
        for (n in names) {
            val fromKey = NameSorting.key(n).firstOrNull { it.isLetterOrDigit() }?.uppercaseChar()
            val expected = if (fromKey != null && fromKey in 'A'..'Z') fromKey else '#'
            assertEquals(
                "rail letter for ${n.trim()} must follow its sort key",
                expected,
                NameSorting.letter(n),
            )
        }
    }

    @Test
    fun `sorting by the key puts each name under the letter the rail shows`() {
        val sorted = names.sortedBy { NameSorting.key(it) }
        // Walking the sorted list, the rail letters must be non-decreasing —
        // otherwise a rail jump lands outside its own section.
        val letters = sorted.map { NameSorting.letter(it) }
        val ordered = letters.filter { it != '#' }
        assertEquals(
            "rail letters must be non-decreasing down the sorted list, got $letters for $sorted",
            ordered.sorted(),
            ordered,
        )
    }

    @Test
    fun `a name beginning with an article keeps it — An is part of the name`() {
        // The case that made F2 visible, and the reason names do not reuse the
        // title sort: "An" is a given name here, not an article to strip.
        assertEquals("na an", NameSorting.key("An Na"))
        assertEquals('N', NameSorting.letter("An Na"))
    }

    /**
     * Surname order (SKADI-T-0648 F1). The same vector is in skadi-web
     * (`audiobooks.rs`, `name_sort_key`), so the two clients order alike.
     */
    @Test
    fun `authors order by surname`() {
        val vector = listOf(
            "Stephen King" to "king stephen",
            "A. G. Riddle" to "riddle a. g.",
            "Ursula K. Le Guin" to "le guin ursula k.",
            "A. E. van Vogt" to "van vogt a. e.",
            "Fritz Leiber Jr." to "leiber fritz",
            "Fritz Leiber, Jr." to "leiber fritz",
            "George R. Martin III" to "martin george r.",
            "Hammett, Dashiell" to "hammett dashiell",
            "Van Morrison" to "morrison van",
            "bell hooks" to "hooks bell",
            "Plato" to "plato",
            "Full Cast" to "full cast",
            "  Alan   Lee " to "lee alan",
        )
        for ((name, key) in vector) assertEquals(name, key, NameSorting.key(name))
        assertEquals('K', NameSorting.letter("Stephen King"))
        assertEquals('L', NameSorting.letter("Ursula K. Le Guin"))
    }

    @Test
    fun `case and surrounding whitespace do not change the order`() {
        assertEquals(NameSorting.key("alan lee"), NameSorting.key("  Alan LEE "))
        assertEquals('L', NameSorting.letter("  alan lee"))
    }

    @Test
    fun `names that do not start with a latin letter bucket together`() {
        assertEquals('#', NameSorting.letter("Йозеф К"))
        assertEquals('#', NameSorting.letter("1984"))
        assertEquals('#', NameSorting.letter(""))
        assertEquals('#', NameSorting.letter("   "))
    }

    @Test
    fun `lowercase author names are not re-cased`() {
        // bell hooks files under H, and must not be title-cased on the way there.
        assertEquals("hooks bell", NameSorting.key("bell hooks"))
        assertEquals('H', NameSorting.letter("bell hooks"))
    }
}
