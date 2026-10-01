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
        "3 Body Problem Author",   // leading digit
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
    fun `a name beginning with an article keeps it — An Na is not Na`() {
        // This is the case that made the bug visible, and the reason names do not
        // reuse the title sort. "An" is part of the name.
        assertEquals("an na", NameSorting.key("An Na"))
        assertEquals('A', NameSorting.letter("An Na"))
    }

    @Test
    fun `case and surrounding whitespace do not change the order`() {
        assertEquals(NameSorting.key("alan lee"), NameSorting.key("  Alan LEE "))
        assertEquals('A', NameSorting.letter("  alan lee"))
    }

    @Test
    fun `names that do not start with a latin letter bucket together`() {
        assertEquals('#', NameSorting.letter("Йозеф К"))
        assertEquals('#', NameSorting.letter("3 Body Problem Author"))
        assertEquals('#', NameSorting.letter(""))
        assertEquals('#', NameSorting.letter("   "))
    }

    @Test
    fun `lowercase author names are not re-cased`() {
        // bell hooks must sort with the Bs and must not be title-cased anywhere
        // on the way there.
        assertEquals("bell hooks", NameSorting.key("bell hooks"))
        assertEquals('B', NameSorting.letter("bell hooks"))
    }
}
