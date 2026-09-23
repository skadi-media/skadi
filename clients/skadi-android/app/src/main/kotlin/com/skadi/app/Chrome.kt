package com.skadi.app

import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.horizontalScroll
import androidx.activity.compose.BackHandler
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.RowScope
import androidx.compose.foundation.layout.aspectRatio
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.Close
import androidx.compose.material.icons.filled.Search
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextField
import androidx.compose.material3.TextFieldDefaults
import androidx.compose.material3.TopAppBar
import androidx.compose.material3.TopAppBarDefaults
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.Shape
import androidx.compose.ui.platform.LocalSoftwareKeyboardController
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import com.skadi.core.Movie
import com.skadi.core.Series

/**
 * Shared screen chrome (SKADI-T-0577).
 *
 * Before this, every screen drew its own header: a headline in the *primary*
 * colour beside a `TextButton` that read "Done" on one screen, "Back" on the
 * next and "← Library" on a third, over a permanent full-height search box. Four
 * screens, four headers, none of them the platform's. The clutter the user saw
 * was mostly that — chrome competing with content, and differently on each
 * screen.
 *
 * One top bar, used everywhere. Search is an icon until it is wanted, and then
 * it *is* the bar: the title gives way to the field, the way every Material app
 * does it, and back (arrow or gesture) closes it again.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun SkadiTopBar(
    title: String,
    /** Current query; `null` means this screen has no search. */
    query: String? = null,
    onQuery: (String) -> Unit = {},
    placeholder: String = "Search",
    onBack: (() -> Unit)? = null,
    actions: @Composable RowScope.() -> Unit = {},
) {
    // Open if a query is already set (e.g. state restored after rotation): a
    // filtered list with no visible reason for the filter is a bug report.
    var searching by rememberSaveable { mutableStateOf(!query.isNullOrEmpty()) }
    val close = { searching = false; onQuery("") }
    BackHandler(enabled = searching, onBack = close)

    val colors = TopAppBarDefaults.topAppBarColors(
        containerColor = MaterialTheme.colorScheme.background,
        titleContentColor = MaterialTheme.colorScheme.onBackground,
        navigationIconContentColor = MaterialTheme.colorScheme.onSurfaceVariant,
        actionIconContentColor = MaterialTheme.colorScheme.onSurfaceVariant,
    )

    if (searching && query != null) {
        val focus = remember { FocusRequester() }
        val keyboard = LocalSoftwareKeyboardController.current
        LaunchedEffect(Unit) { focus.requestFocus() }
        TopAppBar(
            colors = colors,
            navigationIcon = {
                IconButton(onClick = close) {
                    Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = "Close search")
                }
            },
            title = {
                TextField(
                    value = query,
                    onValueChange = onQuery,
                    placeholder = { Text(placeholder) },
                    singleLine = true,
                    keyboardOptions = KeyboardOptions(imeAction = ImeAction.Search),
                    keyboardActions = KeyboardActions(onSearch = { keyboard?.hide() }),
                    // Bare text on the bar's own background — a boxed field inside
                    // an app bar is two containers where one will do.
                    colors = TextFieldDefaults.colors(
                        focusedContainerColor = Color.Transparent,
                        unfocusedContainerColor = Color.Transparent,
                        focusedIndicatorColor = Color.Transparent,
                        unfocusedIndicatorColor = Color.Transparent,
                    ),
                    modifier = Modifier.fillMaxWidth().focusRequester(focus),
                )
            },
            actions = {
                if (query.isNotEmpty()) {
                    IconButton(onClick = { onQuery("") }) {
                        Icon(Icons.Filled.Close, contentDescription = "Clear")
                    }
                }
            },
        )
        return
    }

    TopAppBar(
        colors = colors,
        navigationIcon = {
            if (onBack != null) {
                IconButton(onClick = onBack) {
                    Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = "Back")
                }
            }
        },
        title = {
            Text(
                title,
                style = MaterialTheme.typography.titleLarge,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
        },
        actions = {
            if (query != null) {
                IconButton(onClick = { searching = true }) {
                    Icon(Icons.Filled.Search, contentDescription = "Search")
                }
            }
            actions()
        },
    )
}

/** Centred one-liner for "nothing here" states, in the muted colour. */
@Composable
fun EmptyState(message: String, action: (@Composable () -> Unit)? = null) {
    Box(
        modifier = Modifier.fillMaxSize().padding(32.dp),
        contentAlignment = Alignment.Center,
    ) {
        androidx.compose.foundation.layout.Column(horizontalAlignment = Alignment.CenterHorizontally) {
            Text(
                message,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                style = MaterialTheme.typography.bodyMedium,
                textAlign = androidx.compose.ui.text.style.TextAlign.Center,
            )
            action?.invoke()
        }
    }
}

/**
 * Top-of-screen notice that the daemon stopped answering (SKADI-T-0606).
 * The list underneath stays — stale numbers beat a blank page — and the
 * line says how stale, so the reader can judge whether to trust them.
 */
@Composable
fun StaleBanner(lastGoodMs: Long?) {
    val age = lastGoodMs?.let { ((System.currentTimeMillis() - it) / 1000).coerceAtLeast(0) }
    val when_ = when {
        age == null -> "no data yet"
        age < 60 -> "last update ${age}s ago"
        age < 3600 -> "last update ${age / 60} min ago"
        else -> "last update ${age / 3600} h ago"
    }
    Text(
        "Server unreachable · $when_",
        color = MaterialTheme.colorScheme.onErrorContainer,
        style = MaterialTheme.typography.bodySmall,
        modifier = Modifier
            .fillMaxWidth()
            .background(MaterialTheme.colorScheme.errorContainer)
            .padding(horizontal = 16.dp, vertical = 6.dp),
    )
}

@Composable
fun Loading() {
    Box(modifier = Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
        CircularProgressIndicator()
    }
}

/** A blank thumbnail slot for items with no artwork; poster (2:3) or square. */
@Composable
fun ArtPlaceholder(modifier: Modifier = Modifier, shape: Shape, poster: Boolean = true) {
    Box(
        modifier = modifier
            .then(if (poster) Modifier.aspectRatio(2f / 3f) else Modifier)
            .clip(shape)
            .background(MaterialTheme.colorScheme.surfaceVariant),
    )
}

/**
 * The lists a tab shows, kept for the life of the process.
 *
 * With tabs, a screen leaves composition every time you switch away, and its
 * `remember`ed list with it. Re-fetching 1,818 films on every tap of the Movies
 * tab is a spinner where a list should be. This holds the last result so a tab
 * paints immediately and refreshes behind it.
 */
object LibraryCache {
    @Volatile var movies: List<Movie>? = null

    @Volatile var series: List<Series>? = null

    /** Full series (with episodes) by id, for "next episode" in the player. */
    val seriesDetail: java.util.concurrent.ConcurrentHashMap<String, Series> =
        java.util.concurrent.ConcurrentHashMap()
}


/**
 * Genre chips over a wall (SKADI-T-0605): one chip per genre the loaded
 * items carry, most common first, with counts; tapping the active chip
 * clears it. Hidden entirely when nothing has genres yet (a library the
 * server has not refreshed since genres existed), so the wall looks as it
 * always did rather than growing an empty row.
 */
@androidx.compose.runtime.Composable
fun GenreChips(genresOf: List<List<String>>, selected: String?, onSelect: (String?) -> Unit) {
    val counts = remember(genresOf) {
        genresOf.flatMap { it.distinct() }.groupingBy { it }.eachCount().entries
            .sortedWith(compareByDescending<Map.Entry<String, Int>> { it.value }.thenBy { it.key })
    }
    if (counts.isEmpty()) return
    androidx.compose.foundation.layout.Row(
        horizontalArrangement = androidx.compose.foundation.layout.Arrangement.spacedBy(8.dp),
        modifier = Modifier
            .horizontalScroll(rememberScrollState())
            .padding(horizontal = 16.dp, vertical = 4.dp),
    ) {
        counts.forEach { (name, n) ->
            androidx.compose.material3.FilterChip(
                selected = selected == name,
                onClick = { onSelect(if (selected == name) null else name) },
                label = { Text("$name ($n)") },
            )
        }
    }
}
