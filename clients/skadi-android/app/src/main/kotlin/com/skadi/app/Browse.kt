package com.skadi.app

import androidx.compose.foundation.gestures.detectTapGestures
import androidx.compose.foundation.gestures.detectVerticalDragGestures
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.platform.LocalHapticFeedback
import androidx.compose.ui.hapticfeedback.HapticFeedbackType
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp

/**
 * A–Z jump rail (SKADI-T-0579).
 *
 * A flat scrolling list does not survive this library. 1,818 movies at one row
 * each is roughly **260 screenfuls**; finding "The Thing" means flicking past
 * everything before it. Search helps when you know the title, but browsing is
 * the case search cannot serve — and browsing is most of what a media library is
 * for.
 *
 * Drag as well as tap: dragging down the rail scrubs through letters
 * continuously, which is how iOS contacts and every music app has worked for
 * fifteen years, and is far faster than twenty-six separate taps.
 *
 * Haptics on each letter change, because the rail is narrow and a finger covers
 * it — the feedback is what tells you which letter you are on when you cannot
 * see it.
 */
@Composable
fun AlphabetRail(
    /** First letter of each item, in list order. */
    letters: List<Char>,
    onJump: (index: Int) -> Unit,
    modifier: Modifier = Modifier,
) {
    if (letters.isEmpty()) return
    // First index for each letter present. Only letters that exist are shown:
    // offering "Q" on a library with no Q is a tap that does nothing, and the
    // rail is the one control that must always land somewhere.
    val firstIndex = remember(letters) {
        buildMap {
            letters.forEachIndexed { i, c -> if (!containsKey(c)) put(c, i) }
        }
    }
    val shown = remember(firstIndex) { firstIndex.keys.sorted() }
    if (shown.size < 2) return

    val haptics = LocalHapticFeedback.current
    var active by remember { mutableStateOf<Char?>(null) }

    fun pick(yFraction: Float) {
        val idx = (yFraction * shown.size).toInt().coerceIn(0, shown.size - 1)
        val c = shown[idx]
        if (c != active) {
            active = c
            haptics.performHapticFeedback(HapticFeedbackType.TextHandleMove)
            firstIndex[c]?.let(onJump)
        }
    }

    Column(
        modifier = modifier
            .fillMaxHeight()
            // 32dp, not the 24dp the glyphs need: a rail narrower than a
            // fingertip is a control you can see and cannot hit. Material puts
            // the minimum target at 48dp; 32 is the compromise that keeps a
            // three-column grid from losing a column.
            .width(32.dp)
            // No fill: a filled column down the edge of every list read as a
            // second panel. The letters are the control (SKADI-T-0577).
            .pointerInput(shown) {
                val h = size.height.coerceAtLeast(1).toFloat()
                detectVerticalDragGestures(
                    onDragStart = { pick(it.y / h) },
                    onDragEnd = { active = null },
                    onDragCancel = { active = null },
                ) { change, _ -> pick(change.position.y / h) }
            }
            .pointerInput(shown) {
                val h = size.height.coerceAtLeast(1).toFloat()
                detectTapGestures { pick(it.y / h) }
            },
        verticalArrangement = Arrangement.SpaceEvenly,
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        shown.forEach { c ->
            Text(
                c.toString(),
                style = MaterialTheme.typography.labelSmall,
                textAlign = TextAlign.Center,
                color = if (c == active) {
                    MaterialTheme.colorScheme.primary
                } else {
                    MaterialTheme.colorScheme.onSurfaceVariant
                },
            )
        }
    }
}

/**
 * The letter a title sorts under, for [AlphabetRail].
 *
 * Leading articles are dropped, so *The Matrix* files under **M** — the same
 * thing every library catalogue does, and what someone scanning for it expects.
 * Anything not starting with a letter files under `#`, so numbers and symbols
 * are one bucket rather than scattered.
 */
fun sortLetter(title: String): Char {
    val t = title.trim().lowercase()
    val stripped = listOf("the ", "a ", "an ").firstOrNull { t.startsWith(it) }
        ?.let { t.removePrefix(it) } ?: t
    val c = stripped.firstOrNull()?.uppercaseChar() ?: '#'
    return if (c in 'A'..'Z') c else '#'
}

/** Sort key matching [sortLetter] — same article handling, so the rail's letters
 *  line up with the order actually rendered. */
fun sortKey(title: String): String {
    val t = title.trim().lowercase()
    return listOf("the ", "a ", "an ").firstOrNull { t.startsWith(it) }
        ?.let { t.removePrefix(it) } ?: t
}

/**
 * One tile in the movie grid: a single film, or a collapsed franchise
 * (SKADI-T-0581).
 *
 * Sealed rather than a nullable-collection flag so the grid cannot render a
 * "collection" with one film in it or a "film" with three — the two cases have
 * genuinely different content and a different tap action.
 */
sealed interface MovieTile {
    val sortTitle: String

    data class Single(val movie: com.skadi.core.Movie) : MovieTile {
        override val sortTitle get() = movie.title
    }

    data class Collection(
        val name: String,
        val films: List<com.skadi.core.Movie>,
    ) : MovieTile {
        override val sortTitle get() = name
    }
}

/**
 * Collapse franchises into single tiles.
 *
 * Only groups of **two or more** collapse. TMDB puts plenty of standalone films
 * in a collection of one, and turning those into a folder would add a tap that
 * reveals exactly what the tile already showed.
 *
 * Grouping is by collection **id**, never by title similarity: "101 Dalmatians"
 * and "102 Dalmatians" are a real collection, "10 Cloverfield Lane" is not one
 * with them, and no prefix rule separates those two cases correctly.
 */
fun collapseCollections(movies: List<com.skadi.core.Movie>): List<MovieTile> {
    val (grouped, loose) = movies.partition { it.collection != null }
    val tiles = mutableListOf<MovieTile>()
    loose.forEach { tiles += MovieTile.Single(it) }
    grouped.groupBy { it.collection!!.tmdb_id }.forEach { (_, films) ->
        if (films.size < 2) {
            tiles += MovieTile.Single(films.first())
        } else {
            tiles += MovieTile.Collection(
                name = films.first().collection!!.name,
                // Release order — the order someone would watch them in, and
                // stable, unlike whatever order the API returned.
                films = films.sortedWith(
                    compareBy({ it.year ?: Int.MAX_VALUE }, { it.title }),
                ),
            )
        }
    }
    return tiles.sortedBy { sortKey(it.sortTitle) }
}
