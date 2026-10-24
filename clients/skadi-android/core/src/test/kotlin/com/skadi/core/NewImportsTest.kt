package com.skadi.core

import kotlinx.serialization.json.Json
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/** New imports (SKADI-T-0746): the wire shape and the pure helpers of the New tab. */
class NewImportsTest {
    private val json = Json { ignoreUnknownKeys = true }

    private fun row(id: String, kind: String = "movie", episode: String? = null, title: String? = null) =
        NewImport(id = id, at = "2026-10-24T12:00:00Z", kind = kind, itemId = "item-$id", title = "T $id",
            episode = episode, episodeTitle = title, year = 2024)

    @Test
    fun `a page reads the server's shape`() {
        val page = json.decodeFromString<NewImportsPage>(
            """{"items":[{"id":"h1","at":"2026-10-24T12:00:00Z","kind":"series","quality":"WEBDL-1080p",
               "upgrade":true,"item_id":"s1","title":"Severance","episode":"S01E02",
               "episode_title":"Half Loop","year":2022,"poster_url":"https://img/p.jpg","extra":1}],
               "next_cursor":"2026-10-24T12:00:00Z~h1"}""",
        )
        val n = page.items.single()
        assertEquals("s1", n.itemId)
        assertTrue(n.upgrade)
        assertEquals("S01E02 · Half Loop", n.subtitle())
        assertEquals("TV", n.kindTag())
        assertEquals("2026-10-24T12:00:00Z~h1", page.nextCursor)
        assertNull(json.decodeFromString<NewImportsPage>("""{"items":[]}""").nextCursor)
    }

    @Test
    fun `the path carries the filter and an escaped cursor`() {
        assertEquals("/new-imports?limit=30", newImportsPath(null, null, 30))
        assertEquals(
            "/new-imports?limit=30&kind=series&cursor=2026-10-24T12%3A00%3A00Z%7Eh1",
            newImportsPath("series", "2026-10-24T12:00:00Z~h1", 30),
        )
    }

    @Test
    fun `an episode label gives its season`() {
        assertEquals(3, seasonOf("S03E07"))
        assertEquals(10, seasonOf("S10E01"))
        assertNull(seasonOf("E07"))
        assertNull(seasonOf(null))
        assertNull(seasonOf("Sxx"))
    }

    @Test
    fun `the subtitle and tag of a film and a book`() {
        assertEquals("2024", row("m").subtitle())
        assertEquals("FILM", row("m").kindTag())
        assertEquals("BOOK", row("b", kind = "audiobook").kindTag())
        assertEquals("S01E01", row("e", "series", "S01E01").subtitle())
    }

    @Test
    fun `the badge is the count, capped, and absent at zero`() {
        assertNull(newImportsBadge(0))
        assertEquals("7", newImportsBadge(7))
        assertEquals("99+", newImportsBadge(140))
    }

    @Test
    fun `a refresh puts new rows on top and keeps the rest`() {
        val shown = listOf(row("b"), row("a"))
        val merged = mergeNewer(shown, listOf(row("c"), row("b")))
        assertEquals(listOf("c", "b", "a"), merged.map { it.id })
        assertFalse(merged.groupBy { it.id }.any { it.value.size > 1 })
    }
}
