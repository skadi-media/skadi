package com.skadi.app

import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.rememberScrollState
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.MoreVert
import androidx.compose.material3.Card
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.FilterChip
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.SnackbarHost
import androidx.compose.material3.SnackbarHostState
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import com.skadi.core.Acquirable
import com.skadi.core.SkadiApi
import com.skadi.core.WantedEdition
import com.skadi.core.WantedItem
import com.skadi.core.WantedSummary
import kotlinx.coroutines.launch

/**
 * The backlog (SKADI-T-0602): everything monitored that Skadi has not got a
 * good file for, as the web `/wanted` page shows it — so the phone can prune
 * it (unmonitor an item, an episode, or every series' specials at once) and
 * hunt from it (search now, pick a release, paste a link) without a laptop.
 */
@Composable
fun WantedScreen(
    baseUrl: String,
    token: String?,
    onBack: () -> Unit,
    /** Jump to the TV tab's show page (SKADI-T-0608). */
    onOpenSeries: (seriesId: String, season: Int?) -> Unit = { _, _ -> },
) {
    val api = remember(baseUrl, token) { SkadiApi(baseUrl, token) }
    val scope = rememberCoroutineScope()
    val snackbar = remember { SnackbarHostState() }
    var all by remember { mutableStateOf<List<WantedItem>?>(null) }
    var summary by remember { mutableStateOf(WantedSummary()) }
    var failed by remember { mutableStateOf(false) }
    var kind by rememberSaveable { mutableStateOf("all") }
    var status by rememberSaveable { mutableStateOf("all") }
    var query by rememberSaveable { mutableStateOf("") }
    var reloadKey by remember { mutableStateOf(0) }
    var refreshing by remember { mutableStateOf(false) }
    var hunt by remember { mutableStateOf<Pair<String, String>?>(null) }
    var paste by remember { mutableStateOf<String?>(null) }

    LaunchedEffect(reloadKey) {
        runCatching { api.wanted() }
            .onSuccess { all = it.items; summary = it.summary; failed = false }
            .onFailure { failed = all == null }
        refreshing = false
    }
    fun say(msg: String) = scope.launch { snackbar.showSnackbar(msg) }

    // Optimistic pruning: an unmonitored thing leaves the list at once; the
    // next reload agrees with the server or brings it back.
    fun dropItem(id: String) { all = all?.filter { it.id != id } }
    fun dropEditions(itemId: String, keep: (WantedEdition) -> Boolean) {
        all = all?.mapNotNull { i ->
            if (i.id != itemId) i else i.copy(editions = i.editions.filter(keep)).takeIf { it.editions.isNotEmpty() }
        }
    }

    hunt?.let { (path, what) ->
        ReleasesSheet(api = api, path = path, what = what, onDismiss = { hunt = null }, onGrabbed = { say("Grabbed: $it") })
    }
    paste?.let { path ->
        PasteLinkDialog(
            onDismiss = { paste = null },
            onGrab = { link, title ->
                paste = null
                scope.launch {
                    runCatching { api.grabLink(path, link, title) }
                        .onSuccess { say("Link handed to the worker.") }
                        .onFailure { say(it.message ?: "Couldn't grab that link.") }
                }
            },
        )
    }

    Scaffold(
        containerColor = MaterialTheme.colorScheme.background,
        topBar = { SkadiTopBar(title = "Wanted", query = query, onQuery = { query = it }, placeholder = "Filter titles", onBack = onBack) },
        snackbarHost = { SnackbarHost(snackbar) },
    ) { inset ->
        val items = all
        when {
            items == null && failed -> EmptyState("Couldn't reach the server.") { TextButton(onClick = { reloadKey++ }) { Text("Try again") } }
            items == null -> Loading()
            else -> {
                val shown = filterWanted(items, kind, status, query)
                val specials = items.filter { it.hasSpecials }
                Refreshable(refreshing = refreshing, onRefresh = { refreshing = true; reloadKey++ }) {
LazyColumn(modifier = Modifier.fillMaxSize().padding(inset), contentPadding = PaddingValues(bottom = 24.dp)) {
                    item {
                        val (s, m, a) = items.fold(Triple(0, 0, 0)) { (s, m, a), i ->
                            when (i.kind) { "series" -> Triple(s + 1, m, a); "movie" -> Triple(s, m + 1, a); else -> Triple(s, m, a + 1) }
                        }
                        Row(
                            horizontalArrangement = Arrangement.spacedBy(8.dp),
                            modifier = Modifier.horizontalScroll(rememberScrollState()).padding(horizontal = 16.dp, vertical = 4.dp),
                        ) {
                            FilterChip(selected = kind == "all", onClick = { kind = "all" }, label = { Text("All (${items.size})") })
                            FilterChip(selected = kind == "series", onClick = { kind = "series" }, label = { Text("TV ($s)") })
                            FilterChip(selected = kind == "movie", onClick = { kind = "movie" }, label = { Text("Movies ($m)") })
                            FilterChip(selected = kind == "book", onClick = { kind = "book" }, label = { Text("Audiobooks ($a)") })
                        }
                        Row(
                            horizontalArrangement = Arrangement.spacedBy(8.dp),
                            modifier = Modifier.horizontalScroll(rememberScrollState()).padding(horizontal = 16.dp, vertical = 4.dp),
                        ) {
                            FilterChip(selected = status == "all", onClick = { status = "all" }, label = { Text("Any status") })
                            summary.by_status.entries.sortedByDescending { it.value }.forEach { (k, n) ->
                                FilterChip(selected = status == k, onClick = { status = k }, label = { Text("$k ($n)") })
                            }
                        }
                    }
                    if (specials.isNotEmpty() && (kind == "all" || kind == "series")) {
                        item { SpecialsPrune(specials.size, api, onDone = { n -> say("Specials unmonitored on $n series."); reloadKey++ }) }
                    }
                    if (shown.isEmpty()) {
                        item { EmptyState(if (items.isEmpty()) "Nothing wanted — everything monitored is on disk." else "No match.") }
                    }
                    items(shown, key = { it.kind + it.id }) { item ->
                        WantedCard(
                            item = item,
                            onHunt = { ed -> hunt = acquirablePath(item, ed) to "${item.title} · ${ed.label}" },
                            onSearchNow = { ed ->
                                scope.launch {
                                    runCatching { api.acquire(acquirablePath(item, ed)) }
                                        .onSuccess { say("Search queued for ${item.title} · ${ed.label}.") }
                                        .onFailure { say(it.message ?: "Couldn't queue a search.") }
                                }
                            },
                            onPaste = { ed -> paste = acquirablePath(item, ed) },
                            onUnmonitorEdition = { ed ->
                                if (item.kind == "series") scope.launch {
                                    runCatching { api.monitorEpisode(item.id, ed.id, false) }
                                        .onSuccess { dropEditions(item.id) { it.id != ed.id }; say("${ed.label} unmonitored.") }
                                        .onFailure { say(it.message ?: "Couldn't unmonitor.") }
                                }
                            },
                            onUnmonitorItem = {
                                scope.launch {
                                    runCatching {
                                        when (item.kind) {
                                            "series" -> api.setSeriesMonitored(item.id, false)
                                            "movie" -> api.setMovieMonitored(item.id, false)
                                            else -> api.setBookMonitored(item.id, false)
                                        }
                                    }.onSuccess { dropItem(item.id); say("${item.title} unmonitored.") }
                                        .onFailure { say(it.message ?: "Couldn't unmonitor.") }
                                }
                            },
                            onOpenShow = {
                                val season = item.editions.firstOrNull { !it.isSpecial }?.kind?.drop(1)?.take(2)?.toIntOrNull()
                                onOpenSeries(item.id, season)
                            },
                            onUnmonitorSpecials = {
                                scope.launch {
                                    runCatching { api.monitorSeason(item.id, 0, false) }
                                        .onSuccess { dropEditions(item.id) { !it.isSpecial }; say("Specials unmonitored.") }
                                        .onFailure { say(it.message ?: "Couldn't unmonitor specials.") }
                                }
                            },
                        )
                        HorizontalDivider()
                    }
                }
}
            }
        }
    }
}

/** Same rule as the web page: kind and title filter the item, status filters
 *  its editions, an item with no edition left drops out. */
fun filterWanted(items: List<WantedItem>, kind: String, status: String, q: String): List<WantedItem> {
    val needle = q.trim().lowercase()
    return items.asSequence()
        .filter { kind == "all" || it.kind == kind }
        .filter { needle.isEmpty() || it.title.lowercase().contains(needle) }
        .mapNotNull { i ->
            val eds = i.editions.filter { status == "all" || it.status_kind == status }
            if (eds.isEmpty()) null else i.copy(editions = eds)
        }
        .sortedBy { it.title.lowercase() }
        .toList()
}

private fun acquirablePath(item: WantedItem, ed: WantedEdition): String = when (item.kind) {
    "series" -> Acquirable.episode(item.id, ed.id)
    "movie" -> Acquirable.movieEdition(item.id, ed.id)
    else -> Acquirable.bookFile(item.id, ed.id)
}

/** Bulk prune, two taps apart: specials were most of the TV backlog on prod
 *  and nobody asked for them, but one stray tap must not unmonitor a few
 *  hundred seasons. */
@Composable
private fun SpecialsPrune(count: Int, api: SkadiApi, onDone: (Int) -> Unit) {
    val scope = rememberCoroutineScope()
    var confirm by remember { mutableStateOf(false) }
    var busy by remember { mutableStateOf(false) }
    var progress by remember { mutableStateOf<String?>(null) }
    Card(
        colors = CardDefaults.cardColors(containerColor = MaterialTheme.colorScheme.surfaceVariant),
        modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 8.dp),
    ) {
        Column(modifier = Modifier.padding(12.dp)) {
            Text("$count series are waiting on specials (S00) — usually extras nobody asked for.", style = MaterialTheme.typography.bodyMedium)
            progress?.let { Text(it, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant) }
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp), verticalAlignment = Alignment.CenterVertically) {
                if (!confirm) {
                    TextButton(enabled = !busy, onClick = { confirm = true }) { Text("Unmonitor all specials…") }
                } else {
                    Text("Sure?", style = MaterialTheme.typography.labelLarge)
                    TextButton(enabled = !busy, onClick = {
                        busy = true; confirm = false
                        scope.launch {
                            var done = 0
                            val ids = runCatching { api.wanted().items.filter { it.hasSpecials }.map { it.id } }.getOrDefault(emptyList())
                            ids.forEachIndexed { i, id ->
                                progress = "Unmonitoring specials ${i + 1} / ${ids.size}…"
                                if (runCatching { api.monitorSeason(id, 0, false) }.isSuccess) done++
                            }
                            progress = null; busy = false
                            onDone(done)
                        }
                    }) { Text("Yes, all $count", color = MaterialTheme.colorScheme.error) }
                    TextButton(onClick = { confirm = false }) { Text("Cancel") }
                }
            }
        }
    }
}

private const val EDITIONS_FOLDED = 4

@Composable
private fun WantedCard(
    item: WantedItem,
    onHunt: (WantedEdition) -> Unit,
    onSearchNow: (WantedEdition) -> Unit,
    onPaste: (WantedEdition) -> Unit,
    onUnmonitorEdition: (WantedEdition) -> Unit,
    onUnmonitorItem: () -> Unit,
    onUnmonitorSpecials: () -> Unit,
    onOpenShow: () -> Unit = {},
) {
    var expanded by rememberSaveable(item.id) { mutableStateOf(false) }
    val ordered = if (item.kind == "series") item.editions.sortedBy { it.isSpecial } else item.editions
    val shown = if (expanded) ordered else ordered.take(EDITIONS_FOLDED)
    Column(modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 8.dp)) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Column(modifier = Modifier.weight(1f)) {
                Text(
                    item.year?.let { "${item.title} ($it)" } ?: item.title,
                    style = MaterialTheme.typography.titleSmall, maxLines = 2, overflow = TextOverflow.Ellipsis,
                )
                Text(
                    when (item.kind) { "series" -> "TV"; "movie" -> "Movie"; else -> "Audiobook" } +
                        " · ${item.editions.size} wanted",
                    style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
            var menu by remember { mutableStateOf(false) }
            IconButton(onClick = { menu = true }) { Icon(Icons.Filled.MoreVert, contentDescription = "Item actions") }
            DropdownMenu(expanded = menu, onDismissRequest = { menu = false }) {
                if (item.kind == "series") DropdownMenuItem(text = { Text("Open show") }, onClick = { menu = false; onOpenShow() })
                if (item.hasSpecials) DropdownMenuItem(text = { Text("Unmonitor specials") }, onClick = { menu = false; onUnmonitorSpecials() })
                DropdownMenuItem(text = { Text("Unmonitor ${if (item.kind == "series") "series" else "item"}") }, onClick = { menu = false; onUnmonitorItem() })
            }
        }
        shown.forEach { ed ->
            EditionRow(item, ed, onHunt, onSearchNow, onPaste, onUnmonitorEdition)
        }
        if (ordered.size > EDITIONS_FOLDED) {
            TextButton(onClick = { expanded = !expanded }) {
                Text(if (expanded) "Show fewer" else "Show all ${ordered.size}")
            }
        }
    }
}

@Composable
private fun EditionRow(
    item: WantedItem,
    ed: WantedEdition,
    onHunt: (WantedEdition) -> Unit,
    onSearchNow: (WantedEdition) -> Unit,
    onPaste: (WantedEdition) -> Unit,
    onUnmonitorEdition: (WantedEdition) -> Unit,
) {
    Row(verticalAlignment = Alignment.CenterVertically, modifier = Modifier.fillMaxWidth()) {
        Text(
            listOfNotNull(ed.label.takeIf { it.isNotBlank() }, ed.status_kind, ed.quality_name).joinToString(" · "),
            style = MaterialTheme.typography.bodyMedium,
            modifier = Modifier.weight(1f), maxLines = 1, overflow = TextOverflow.Ellipsis,
        )
        TextButton(onClick = { onHunt(ed) }) { Text("Releases") }
        var menu by remember { mutableStateOf(false) }
        IconButton(onClick = { menu = true }) { Icon(Icons.Filled.MoreVert, contentDescription = "Actions for ${ed.label}") }
        DropdownMenu(expanded = menu, onDismissRequest = { menu = false }) {
            DropdownMenuItem(text = { Text("Search now") }, onClick = { menu = false; onSearchNow(ed) })
            DropdownMenuItem(text = { Text("Paste a link") }, onClick = { menu = false; onPaste(ed) })
            if (item.kind == "series") DropdownMenuItem(text = { Text("Unmonitor episode") }, onClick = { menu = false; onUnmonitorEdition(ed) })
        }
    }
    Spacer(Modifier.padding(1.dp))
}
