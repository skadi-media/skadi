package com.skadi.core

import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The rule that matters: in the Downloaded view the only delete on offer is the
 * local one — for **every** role, operator included (SKADI-T-0646).
 */
class BookActionPolicyTest {
    private val roles = listOf(true, false) // isAdmin
    private val libraryModes = listOf("all", "series", "authors")

    @Test
    fun `the downloaded view never offers a server-side delete, not even to the operator`() {
        for (isAdmin in roles) {
            assertFalse(
                "isAdmin=$isAdmin: the Downloaded list is scoped to this phone, so a " +
                    "delete that reaches the server must not be reachable from it",
                BookActionPolicy.offersLibraryDelete(isAdmin, "downloaded"),
            )
        }
    }

    @Test
    fun `library views still offer the operator a library delete`() {
        for (mode in libraryModes) {
            assertTrue(mode, BookActionPolicy.offersLibraryDelete(isAdmin = true, mode = mode))
        }
    }

    @Test
    fun `a non-admin is never offered a library delete anywhere`() {
        for (mode in libraryModes + "downloaded") {
            assertFalse(mode, BookActionPolicy.offersLibraryDelete(isAdmin = false, mode = mode))
        }
    }

    @Test
    fun `removing a download is offered to every role, and only when one exists`() {
        assertTrue(BookActionPolicy.offersRemoveDownload(isDownloaded = true))
        assertFalse(BookActionPolicy.offersRemoveDownload(isDownloaded = false))
    }

    @Test
    fun `a member with a download gets a sheet and a member without one does not`() {
        assertTrue(BookActionPolicy.sheetIsUseful(isAdmin = false, isDownloaded = true))
        assertFalse(BookActionPolicy.sheetIsUseful(isAdmin = false, isDownloaded = false))
        assertTrue(BookActionPolicy.sheetIsUseful(isAdmin = true, isDownloaded = false))
    }

    @Test
    fun `watch controls stay operator-only`() {
        assertTrue(BookActionPolicy.offersWatchControls(isAdmin = true))
        assertFalse(BookActionPolicy.offersWatchControls(isAdmin = false))
    }
}
