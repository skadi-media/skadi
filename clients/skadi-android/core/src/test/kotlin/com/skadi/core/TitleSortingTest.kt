package com.skadi.core

import org.junit.Assert.assertEquals
import org.junit.Test

/**
 * One title rule, and the rail cannot drift from it (SKADI-T-0648 F3). The
 * vector here is the one skadi-web asserts (`sorting.rs`), so the two clients
 * provably order alike.
 */
class TitleSortingTest {
    /** Title, key, rail letter — listed in the order both clients must produce. */
    private val vector = listOf(
        Triple("1984", "1984", '#'),
        Triple("A.I. Artificial Intelligence", "a.i. artificial intelligence", 'A'),
        Triple("Anathem", "anathem", 'A'),
        Triple("Dune", "dune", 'D'),
        Triple("A Game of Thrones", "game of thrones", 'G'),
        Triple("The Hobbit", "hobbit", 'H'),
        Triple("  the martian ", "martian", 'M'),
        Triple("Thelma & Louise", "thelma & louise", 'T'),
        Triple("Them!", "them!", 'T'),
        Triple("An Unkindness of Ghosts", "unkindness of ghosts", 'U'),
        Triple("¡Three Amigos!", "¡three amigos!", '#'),
    )

    @Test
    fun `shared title vector keys and letters`() {
        for ((title, key, letter) in vector) {
            assertEquals(title, key, TitleSorting.key(title))
            assertEquals(title, letter, TitleSorting.letter(title))
        }
    }

    @Test
    fun `shared title vector orders as listed`() {
        val shuffled = vector.map { it.first }.reversed().toMutableList()
        shuffled[2] = shuffled[7].also { shuffled[7] = shuffled[2] }
        val railed = TitleSorting.railed(shuffled) { it }
        assertEquals(vector.map { it.first }, railed.items)
        assertEquals(vector.map { it.third }, railed.letters)
    }

    @Test
    fun `letter follows the key`() {
        for ((title, _, _) in vector) {
            val c = TitleSorting.key(title).firstOrNull()?.uppercaseChar()
            val fromKey = if (c != null && c in 'A'..'Z') c else '#'
            assertEquals(title, fromKey, TitleSorting.letter(title))
        }
        assertEquals('#', TitleSorting.letter(""))
        assertEquals('#', TitleSorting.letter("   "))
        assertEquals('#', TitleSorting.letter("The 100"))
    }

    @Test
    fun `names keep their article`() {
        // The title rule and the name rule are different rules on purpose.
        assertEquals("na", TitleSorting.key("An Na"))
        assertEquals("na an", NameSorting.key("An Na"))
    }
}
