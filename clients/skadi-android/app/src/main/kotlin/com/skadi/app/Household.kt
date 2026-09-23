package com.skadi.app

import androidx.compose.runtime.Composable
import androidx.compose.runtime.compositionLocalOf
import com.skadi.core.NotAllowedException

/**
 * Who is holding the phone (SKADI-T-0614). The operator sees the controller
 * half of the app; a member or kid sees the library and the player only. The
 * server enforces all of it (SKADI-T-0612) — hiding controls here is about
 * not showing a child buttons that would only answer "not allowed".
 *
 * Provided once at the paired root from the cached `/me` role; `admin` until
 * the server has said otherwise for a pairing made before roles existed, so
 * the operator's own phone never loses its controls during the upgrade.
 */
val LocalRole = compositionLocalOf { "admin" }

@Composable
fun isAdmin(): Boolean = LocalRole.current == "admin"

/**
 * May this account add media and start a search? (SKADI-T-0625)
 *
 * The operator and a contributor can. A read-only member and a kid cannot —
 * for them the Add button and the hunt controls are simply not there, rather
 * than being there and answering "not allowed".
 */
@Composable
fun canContribute(): Boolean = LocalRole.current in setOf("admin", "contributor")

/** The message a person should read for a failed request: the server's
 *  refusal by name, the exception's own message otherwise. */
fun friendlyError(e: Throwable, fallback: String): String = when (e) {
    is NotAllowedException -> e.message ?: fallback
    else -> e.message ?: fallback
}
