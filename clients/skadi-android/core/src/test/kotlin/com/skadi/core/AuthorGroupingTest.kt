package com.skadi.core

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

/** The authors view groups by record when a book is linked (SKADI-T-0655). */
class AuthorGroupingTest {
    /** A book with the keys a current server emits for it (SKADI-T-0737). */
    private fun book(
        id: String,
        title: String,
        authors: List<String>,
        authorId: String? = null,
        titleKey: String? = null,
        authorKeys: List<String> = emptyList(),
    ) = Book(id = id, title = title, authors = authors, authorId = authorId, sortTitle = titleKey, sortAuthors = authorKeys)

    @Test
    fun `two linked authors with one name are two rows, told apart by a hint`() {
        val groups = groupBooksByAuthor(
            listOf(
                book("1", "The Stand", listOf("Stephen King"), "a1", "stand", listOf("king stephen")),
                book("2", "Misery", listOf("Stephen King"), "a1", "misery", listOf("king stephen")),
                book("3", "Prose and Cons", listOf("Stephen King"), "a2", "prose and cons", listOf("king stephen")),
                // Unlinked, and two rows carry the name: keeps its own row.
                book("4", "Carrie", listOf("Stephen  King"), null, "carrie", listOf("king stephen")),
                // Unlinked, one row carries the name: folds into it.
                book("5", "Dune", listOf("Frank Herbert"), "h1", "dune", listOf("herbert frank")),
                book("6", "Dune Messiah", listOf("Frank Herbert"), null, "dune messiah", listOf("herbert frank")),
            ),
        )
        assertEquals(
            listOf(
                "id:h1" to listOf("5", "6"),
                "id:a1" to listOf("2", "1"),
                "id:a2" to listOf("3"),
                "name:stephenking" to listOf("4"),
            ),
            groups.map { g -> g.key to g.books.map { it.id } },
        )
        assertEquals(listOf(null, "e.g. Misery", "e.g. Prose and Cons", "e.g. Carrie"), groups.map { it.hint })
        assertEquals("a1", groups[1].authorId)
        assertNull(groups[3].authorId)
    }

    @Test
    fun `a co-author credit lists the book under both names`() {
        val groups = groupBooksByAuthor(
            listOf(
                book("1", "Good Omens", listOf("Terry Pratchett", "Neil Gaiman"), "p1", "good omens", listOf("pratchett terry", "gaiman neil")),
                book("2", "Coraline", listOf("Neil Gaiman"), "g1", "coraline", listOf("gaiman neil")),
            ),
        )
        assertEquals(listOf("id:g1" to listOf("2", "1"), "id:p1" to listOf("1")), groups.map { g -> g.key to g.books.map { it.id } })
        assertEquals(listOf(null, null), groups.map { it.hint })
    }

    @Test
    fun `unlinked credits group by name key, in surname order`() {
        val groups = groupBooksByAuthor(
            listOf(
                book("1", "The Martian", listOf("Andy Weir"), null, "martian", listOf("weir andy")),
                book("2", "Dune", listOf("Frank Herbert"), null, "dune", listOf("herbert frank")),
                book("3", "Project Hail Mary", listOf("andy  weir"), null, "project hail mary", listOf("weir andy")),
                book("4", "Blank", listOf("", "  ")),
            ),
        )
        assertEquals(listOf("name:frankherbert", "name:andyweir"), groups.map { it.key })
        assertEquals(listOf("Frank Herbert", "Andy Weir"), groups.map { it.name })
        assertEquals(listOf("herbert frank", "weir andy"), groups.map { it.sortKey })
        // The server's title key drops the article: "The Martian" files under M.
        assertEquals(listOf("The Martian", "Project Hail Mary"), groups[1].books.map { it.title })
    }

    @Test
    fun `books from an older server order by the raw name and title`() {
        val groups = groupBooksByAuthor(
            listOf(
                book("1", "The Martian", listOf("Andy Weir")),
                book("2", "Dune", listOf("Frank Herbert")),
                book("3", "Artemis", listOf("Andy Weir")),
            ),
        )
        assertEquals(listOf("Andy Weir", "Frank Herbert"), groups.map { it.name })
        assertEquals(listOf("Artemis", "The Martian"), groups[0].books.map { it.title })
    }
}
