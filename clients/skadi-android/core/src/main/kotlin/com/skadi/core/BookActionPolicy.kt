package com.skadi.core

/**
 * Which destructive actions a book's long-press sheet may offer (SKADI-T-0646).
 *
 * Pulled out as pure functions because the rule is the whole point and it was
 * previously a boolean expression buried in a Compose call site, where the only
 * way to check it was to obtain an account of each role and look. The API
 * refuses to mint a second admin — "there is one admin: the operator" — so the
 * admin path could not be driven on an emulator at all, and the rule that most
 * needs proving was the one that could not be tested.
 */
object BookActionPolicy {
    /** The library view whose contents are scoped to this device. */
    const val DOWNLOADED_MODE = "downloaded"

    /**
     * Removing the on-device copy. Offered whenever there *is* one, for every
     * role: reclaiming space on your own phone is not a controller action, and
     * gating the whole sheet behind the operator role left a member unable to
     * do it.
     */
    @JvmStatic
    fun offersRemoveDownload(isDownloaded: Boolean): Boolean = isDownloaded

    /**
     * Deleting the book from the skadi library — a server-side change.
     *
     * Operator only, **and never from the Downloaded view**: that list is about
     * what is on this phone, so the destructive action reachable from it is
     * scoped to this phone. Library management is not a phone-first job, and the
     * mode check deliberately does not depend on the role — an operator standing
     * in a device-scoped list should not be offered a server-side delete either.
     */
    @JvmStatic
    fun offersLibraryDelete(isAdmin: Boolean, mode: String): Boolean =
        isAdmin && mode != DOWNLOADED_MODE

    /** Watch author/series are controller calls, so operator only. */
    @JvmStatic
    fun offersWatchControls(isAdmin: Boolean): Boolean = isAdmin

    /** Whether the sheet is worth opening at all. */
    @JvmStatic
    fun sheetIsUseful(isAdmin: Boolean, isDownloaded: Boolean): Boolean =
        isAdmin || isDownloaded
}
