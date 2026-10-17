package com.skadi.core

import org.junit.Assert.assertEquals
import org.junit.Test

/**
 * The shared `name_key` vector (SKADI-T-0648 F4): asserted verbatim by the
 * server (`skadi_audiobooks::discovery`) and skadi-web (`author_key`), so the
 * three copies are tested to agree, not said to.
 */
class NameKeyTest {
    @Test
    fun `name key shared vector`() {
        val vector = listOf(
            "A.G. Riddle" to "agriddle",
            "A. G. Riddle" to "agriddle",
            "a g riddle" to "agriddle",
            "Brandon  Sanderson" to "brandonsanderson",
            "Ursula K. Le Guin" to "ursulakleguin",
            "George R. R. Martin" to "georgerrmartin",
            "Ann Leckie" to "annleckie",
            "Anne Leckie" to "anneleckie",
            // Non-ASCII: dropped, not transliterated. "İ" lowercases to "i" plus
            // a combining dot, so lowercasing first would keep an "i" the server drops.
            "İsmail Kadare" to "smailkadare",
            "Émile Zola" to "milezola",
            "Zoë Heller" to "zoheller",
            "Йозеф К" to "",
            "" to "",
            "  " to "",
        )
        for ((name, key) in vector) assertEquals(name, key, nameKey(name))
    }
}
