package com.skadi.app

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Link
import androidx.compose.material.icons.filled.ManageSearch
import androidx.compose.material.icons.filled.Search
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.FilledTonalButton
import androidx.compose.material3.FilterChip
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import com.skadi.core.ReleaseCandidate
import com.skadi.core.SkadiApi
import kotlinx.coroutines.launch

/**
 * Manual hunting from a detail page (SKADI-T-0602): the three verbs the web
 * UI offers on anything acquirable — queue a search, pick a release by hand,
 * paste a link — plus the monitor toggle that takes it off the backlog.
 *
 * [path] is the acquirable's API prefix (see `Acquirable`); [what] names it in
 * the sheet header. [onSetMonitored] performs the toggle; the panel owns the
 * displayed state so the parent's stale model does not fight it.
 */
@Composable
fun HuntPanel(
    api: SkadiApi,
    path: String,
    what: String,
    monitored: Boolean,
    onSetMonitored: suspend (Boolean) -> Unit,
    extra: @Composable ColumnScope.(notify: (String) -> Unit) -> Unit = {},
) {
    // The hunt is the operator's (SKADI-T-0614): a member's page is title,
    // Play and the synopsis, and nothing that would answer "not allowed".
    //
    // A contributor sits between the two (SKADI-T-0625): they get "Search now",
    // because asking skadi to find something is the point of the role, but not
    // the release list, the paste-a-link box or the monitor toggle. Those are
    // the operator's tools, and the release list in particular is where a wrong
    // grab comes from.
    val fullControl = isAdmin()
    if (!fullControl && !canContribute()) return
    val scope = rememberCoroutineScope()
    var isMonitored by remember(path) { mutableStateOf(monitored) }
    var busy by remember { mutableStateOf(false) }
    var note by remember { mutableStateOf<String?>(null) }
    var showReleases by remember { mutableStateOf(false) }
    var showPaste by remember { mutableStateOf(false) }

    Column(modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 8.dp)) {
        Row(horizontalArrangement = Arrangement.spacedBy(8.dp), modifier = Modifier.fillMaxWidth()) {
            FilledTonalButton(
                enabled = !busy,
                modifier = Modifier.weight(1f),
                onClick = {
                    busy = true
                    scope.launch {
                        note = runCatching { api.acquire(path) }
                            .fold({ "Search queued — the hunter picks it up on its next pass." }, { it.message ?: "Couldn't queue a search." })
                        busy = false
                    }
                },
            ) {
                Icon(Icons.Filled.Search, contentDescription = null, modifier = Modifier.padding(end = 6.dp))
                Text("Search now", maxLines = 1)
            }
            if (fullControl) {
                FilledTonalButton(modifier = Modifier.weight(1f), onClick = { showReleases = true }) {
                    Icon(Icons.Filled.ManageSearch, contentDescription = null, modifier = Modifier.padding(end = 6.dp))
                    Text("Releases", maxLines = 1)
                }
            }
        }
        if (fullControl) Row(verticalAlignment = Alignment.CenterVertically, modifier = Modifier.fillMaxWidth()) {
            TextButton(onClick = { showPaste = true }) {
                Icon(Icons.Filled.Link, contentDescription = null, modifier = Modifier.padding(end = 6.dp))
                Text("Paste a link")
            }
            Spacer(Modifier.weight(1f))
            TextButton(
                enabled = !busy,
                onClick = {
                    busy = true
                    val next = !isMonitored
                    scope.launch {
                        runCatching { onSetMonitored(next) }
                            .onSuccess { isMonitored = next; note = if (next) "Monitored — back on the wanted list." else "Unmonitored — Skadi stops looking." }
                            .onFailure { note = it.message ?: "Couldn't change monitoring." }
                        busy = false
                    }
                },
            ) { Text(if (isMonitored) "Unmonitor" else "Monitor") }
        }
        // The caller's extra slot is the season monitor toggle — an operator
        // control, so a contributor does not get it (SKADI-T-0625).
        if (fullControl) extra { note = it }
        note?.let {
            Text(
                it,
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.padding(top = 2.dp),
            )
        }
    }

    if (showReleases) {
        ReleasesSheet(api = api, path = path, what = what, onDismiss = { showReleases = false }, onGrabbed = { note = "Grabbed: $it" })
    }
    if (showPaste) {
        PasteLinkDialog(
            onDismiss = { showPaste = false },
            onGrab = { link, title ->
                showPaste = false
                scope.launch {
                    note = runCatching { api.grabLink(path, link, title) }
                        .fold({ "Link handed to the worker." }, { it.message ?: "Couldn't grab that link." })
                }
            },
        )
    }
}

/**
 * Interactive search results for one acquirable, newest search on open. Rows
 * arrive sorted by relevance; accepted ones (would be auto-grabbed) lead. A
 * rejected row can still be grabbed — reachability beats the profile when the
 * operator says so — its reason is shown under the title so that is informed.
 */
@OptIn(androidx.compose.material3.ExperimentalMaterial3Api::class)
@Composable
fun ReleasesSheet(api: SkadiApi, path: String, what: String, onDismiss: () -> Unit, onGrabbed: (String) -> Unit) {
    val scope = rememberCoroutineScope()
    var rows by remember(path) { mutableStateOf<List<ReleaseCandidate>?>(null) }
    var error by remember(path) { mutableStateOf<String?>(null) }
    var onlyAccepted by remember(path) { mutableStateOf(true) }
    var grabbing by remember { mutableStateOf<String?>(null) }
    var grabbed by remember { mutableStateOf(setOf<String>()) }
    var grabError by remember { mutableStateOf<String?>(null) }

    LaunchedEffect(path) {
        runCatching { api.listReleases(path) }
            .onSuccess { r ->
                rows = r.sortedByDescending { it.accepted }
                if (r.none { it.accepted }) onlyAccepted = false
            }
            .onFailure { error = it.message ?: "Search failed." }
    }

    ModalBottomSheet(onDismissRequest = onDismiss) {
        Text(
            what,
            style = MaterialTheme.typography.titleMedium,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
            modifier = Modifier.padding(horizontal = 16.dp),
        )
        val list = rows
        when {
            error != null -> Text(error!!, modifier = Modifier.padding(16.dp), color = MaterialTheme.colorScheme.error)
            list == null -> Row(verticalAlignment = Alignment.CenterVertically, modifier = Modifier.padding(16.dp)) {
                CircularProgressIndicator(modifier = Modifier.width(24.dp).height(24.dp))
                Text(
                    "Asking every indexer… this can take a minute.",
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.padding(start = 12.dp),
                )
            }
            list.isEmpty() -> Text("Nothing found on any indexer.", modifier = Modifier.padding(16.dp))
            else -> {
                val accepted = list.count { it.accepted }
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp), modifier = Modifier.padding(horizontal = 16.dp, vertical = 6.dp)) {
                    FilterChip(selected = onlyAccepted, onClick = { onlyAccepted = true }, label = { Text("Accepted ($accepted)") })
                    FilterChip(selected = !onlyAccepted, onClick = { onlyAccepted = false }, label = { Text("All (${list.size})") })
                }
                grabError?.let { Text(it, color = MaterialTheme.colorScheme.error, style = MaterialTheme.typography.bodySmall, modifier = Modifier.padding(horizontal = 16.dp)) }
                val shown = if (onlyAccepted) list.filter { it.accepted } else list
                LazyColumn(modifier = Modifier.fillMaxWidth()) {
                    // Keyed by position, not release_key: two indexers can
                    // list the same infohash and a duplicate key crashes the list.
                    itemsIndexed(shown, key = { i, r -> "$i:${r.release_key}" }) { _, r ->
                        ReleaseRow(
                            r = r,
                            state = when {
                                r.release_key in grabbed -> RowState.Grabbed
                                grabbing == r.release_key -> RowState.Grabbing
                                else -> RowState.Idle
                            },
                            onGrab = {
                                grabbing = r.release_key
                                grabError = null
                                scope.launch {
                                    runCatching { api.grabRelease(path, r.release) }
                                        .onSuccess { grabbed = grabbed + r.release_key; onGrabbed(r.title) }
                                        .onFailure { grabError = it.message ?: "Grab refused." }
                                    grabbing = null
                                }
                            },
                        )
                        HorizontalDivider()
                    }
                    item { Spacer(Modifier.height(24.dp)) }
                }
            }
        }
    }
}

private enum class RowState { Idle, Grabbing, Grabbed }

@Composable
private fun ReleaseRow(r: ReleaseCandidate, state: RowState, onGrab: () -> Unit) {
    Row(verticalAlignment = Alignment.CenterVertically, modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 8.dp)) {
        Column(modifier = Modifier.weight(1f)) {
            Text(r.title, style = MaterialTheme.typography.bodyMedium, maxLines = 2, overflow = TextOverflow.Ellipsis)
            val meta = listOfNotNull(
                r.quality.takeIf { it.isNotBlank() },
                r.sizeBytes?.let { releaseSize(it) },
                r.seeders?.let { "$it seeders" },
                "${r.age_days}d",
                r.indexer,
            ).joinToString(" · ")
            Text(meta, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
            val flag = when {
                r.season_pack && r.accepted -> "Season pack — fills the whole season"
                r.season_pack -> "Season pack · ${r.reason}"
                !r.accepted -> r.reason
                else -> null
            }
            flag?.takeIf { it.isNotBlank() }?.let {
                Text(
                    it,
                    style = MaterialTheme.typography.bodySmall,
                    color = if (r.accepted) MaterialTheme.colorScheme.primary else MaterialTheme.colorScheme.tertiary,
                    maxLines = 2,
                    overflow = TextOverflow.Ellipsis,
                )
            }
        }
        Spacer(Modifier.width(8.dp))
        when (state) {
            RowState.Grabbed -> Text("Grabbed", style = MaterialTheme.typography.labelLarge, color = MaterialTheme.colorScheme.primary)
            RowState.Grabbing -> CircularProgressIndicator(modifier = Modifier.width(20.dp).height(20.dp))
            RowState.Idle -> OutlinedButton(onClick = onGrab) { Text("Grab") }
        }
    }
}

@Composable
fun PasteLinkDialog(onDismiss: () -> Unit, onGrab: (link: String, title: String?) -> Unit) {
    var link by remember { mutableStateOf("") }
    var title by remember { mutableStateOf("") }
    val ok = link.trim().let { it.startsWith("magnet:") || it.startsWith("http://") || it.startsWith("https://") }
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text("Paste a link") },
        text = {
            Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                OutlinedTextField(
                    value = link, onValueChange = { link = it },
                    label = { Text("Magnet or .torrent URL") }, singleLine = true,
                    modifier = Modifier.fillMaxWidth(),
                )
                OutlinedTextField(
                    value = title, onValueChange = { title = it },
                    label = { Text("Release title (optional)") }, singleLine = true,
                    modifier = Modifier.fillMaxWidth(),
                )
            }
        },
        confirmButton = { TextButton(enabled = ok, onClick = { onGrab(link.trim(), title.trim().ifEmpty { null }) }) { Text("Grab") } },
        dismissButton = { TextButton(onClick = onDismiss) { Text("Cancel") } },
    )
}

private fun releaseSize(bytes: Long): String {
    val g = bytes / 1_073_741_824.0
    if (g >= 1) return String.format(java.util.Locale.US, "%.1f GiB", g)
    val m = bytes / 1_048_576.0
    return String.format(java.util.Locale.US, "%.0f MiB", m)
}
