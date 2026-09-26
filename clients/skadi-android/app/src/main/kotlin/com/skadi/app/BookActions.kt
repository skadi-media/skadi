package com.skadi.app

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.selection.toggleable
import androidx.compose.foundation.layout.Row
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Checkbox
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.Switch
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
import com.skadi.core.Book
import com.skadi.core.SkadiApi
import kotlinx.coroutines.launch

/** Normalized author/series key — lowercase, ASCII-alphanumeric only — so a book's
 *  "A.G. Riddle" matches the registered "A. G. Riddle". Mirrors the server's
 *  `name_key` / web `author_key` (SKADI-T-0363). */
/**
 * Per-book library management (SKADI-I-0052, Track B): long-press a book to watch
 * its author/series (auto-acquire future releases) or delete a bad grab. The book
 * payload carries author/series *names* only, so watch keys are resolved against
 * `/authors` and `/audiobooks/series` (name → ASIN) on open.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun BookActionsSheet(
    book: Book,
    api: SkadiApi,
    /**
     * The on-device file id when this book is downloaded, else null. Passed in
     * rather than looked up here: the caller already knows, and a sheet that
     * reaches for its own store starts owning state it cannot keep in step with
     * the tile behind it.
     */
    downloadedFid: String?,
    /**
     * Whether to offer `Delete from library…`. False in the library's
     * **Downloaded** view: that list is about what is on this phone, so the
     * destructive action reachable there must be scoped to this phone
     * (SKADI-T-0646). Removing a book from the library is not a phone-first job.
     */
    offerLibraryDelete: Boolean,
    /**
     * Whether to show the watch-author/series toggles. They are controller calls,
     * so operator only — but removing a download is not, and gating the whole
     * sheet on the operator role left a member unable to reclaim space on their
     * own phone (SKADI-T-0646).
     */
    offerWatchControls: Boolean,
    onRemoveDownload: (String) -> Unit,
    onDismiss: () -> Unit,
    onDeleted: () -> Unit,
) {
    val scope = rememberCoroutineScope()
    val authorName = book.authors.firstOrNull()?.takeIf { it.isNotBlank() }
    val seriesName = book.series?.name?.takeIf { it.isNotBlank() }

    var authorAsin by remember { mutableStateOf<String?>(null) }
    var seriesAsin by remember { mutableStateOf<String?>(null) }
    var watchingAuthor by remember { mutableStateOf(false) }
    var watchingSeries by remember { mutableStateOf(false) }
    var resolved by remember { mutableStateOf(false) }
    var busy by remember { mutableStateOf(false) }
    var confirmDelete by remember { mutableStateOf(false) }
    // Defaults to **false** (SKADI-T-0646). It used to default to true, which
    // made an irreversible server-side file deletion the thing that happens when
    // someone confirms a dialog without reading the checkbox. Opt in to that.
    var deleteFiles by remember { mutableStateOf(false) }
    var confirmRemoveDownload by remember { mutableStateOf(false) }

    LaunchedEffect(book.id, offerWatchControls) {
        if (!offerWatchControls) return@LaunchedEffect
        runCatching {
            val authors = api.listAuthors()
            val series = if (seriesName != null) api.listBookSeries() else emptyList()
            val watchers = api.listWatchers()
            authorAsin = authorName?.let { n ->
                authors.firstOrNull { it.asin != null && nameKey(it.name) == nameKey(n) }?.asin
            }
            seriesAsin = seriesName?.let { n ->
                series.firstOrNull { it.seriesAsin != null && nameKey(it.name) == nameKey(n) }?.seriesAsin
            }
            watchingAuthor = authorAsin?.let { a -> watchers.any { it.scope == "author" && it.key == a } } ?: false
            watchingSeries = seriesAsin?.let { a -> watchers.any { it.scope == "series" && it.key == a } } ?: false
        }
        resolved = true
    }

    fun toggleWatch(scopeName: String, asin: String?, now: Boolean, set: (Boolean) -> Unit) {
        if (asin == null || busy) return
        busy = true
        set(!now) // optimistic
        scope.launch {
            val ok = if (now) api.clearWatcher(scopeName, asin) else api.setWatcher(scopeName, asin)
            if (!ok) set(now) // revert on failure
            busy = false
        }
    }

    if (confirmRemoveDownload && downloadedFid != null) {
        AlertDialog(
            onDismissRequest = { confirmRemoveDownload = false },
            title = { Text("Remove download?") },
            // Names the two things people worry about — the server copy and the
            // library entry — instead of leaving them to infer it from silence.
            text = {
                Text(
                    "“${book.title}” is removed from this phone. It stays in your " +
                        "library and on the server, and you can download it again.",
                )
            },
            confirmButton = {
                TextButton(onClick = {
                    confirmRemoveDownload = false
                    onRemoveDownload(downloadedFid)
                    onDismiss()
                }) { Text("Remove") }
            },
            dismissButton = {
                TextButton(onClick = { confirmRemoveDownload = false }) { Text("Cancel") }
            },
        )
    }

    if (confirmDelete) {
        AlertDialog(
            onDismissRequest = { confirmDelete = false },
            title = { Text("Delete from library?") },
            text = {
                Column {
                    Text("Remove “${book.title}” from the skadi library. This can't be undone.")
                    Row(
                        modifier = Modifier
                            .fillMaxWidth()
                            .padding(top = 12.dp)
                            .toggleable(value = deleteFiles, onValueChange = { deleteFiles = it }),
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        Checkbox(checked = deleteFiles, onCheckedChange = { deleteFiles = it })
                        Text("Also delete the files on the server", modifier = Modifier.padding(start = 4.dp))
                    }
                }
            },
            confirmButton = {
                TextButton(onClick = {
                    val id = book.id
                    val files = deleteFiles
                    confirmDelete = false
                    scope.launch {
                        val ok = api.deleteBook(id, files)
                        if (ok) { onDeleted(); onDismiss() }
                    }
                }) { Text("Delete", color = MaterialTheme.colorScheme.error) }
            },
            dismissButton = { TextButton(onClick = { confirmDelete = false }) { Text("Cancel") } },
        )
    }

    ModalBottomSheet(onDismissRequest = onDismiss) {
        Column(modifier = Modifier.fillMaxWidth().padding(horizontal = 20.dp, vertical = 8.dp)) {
            Text(
                book.title,
                style = MaterialTheme.typography.titleMedium,
                color = MaterialTheme.colorScheme.onSurface,
                maxLines = 2,
                overflow = TextOverflow.Ellipsis,
            )
            val sub = listOfNotNull(authorName, seriesName).joinToString(" · ")
            if (sub.isNotEmpty()) {
                Text(sub, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
            }
            HorizontalDivider(modifier = Modifier.padding(vertical = 12.dp))

            if (offerWatchControls && authorName != null) {
                WatchRow(
                    label = "Watch author",
                    detail = authorName + if (resolved && authorAsin == null) " — not on Audible" else "",
                    checked = watchingAuthor,
                    enabled = authorAsin != null && !busy,
                    onToggle = { toggleWatch("author", authorAsin, watchingAuthor) { watchingAuthor = it } },
                )
            }
            if (offerWatchControls && seriesName != null) {
                WatchRow(
                    label = "Watch series",
                    detail = seriesName + if (resolved && seriesAsin == null) " — no series match" else "",
                    checked = watchingSeries,
                    enabled = seriesAsin != null && !busy,
                    onToggle = { toggleWatch("series", seriesAsin, watchingSeries) { watchingSeries = it } },
                )
            }

            // Local first, and separated by its own divider: in a Downloaded list
            // this is the action people mean, and it is the safe one. Keeping the
            // two structurally apart — rather than one "Delete…" entry with
            // options — is deliberate; the combined form is what produced a
            // pre-checked server-file deletion (SKADI-T-0646).
            if (downloadedFid != null) {
                HorizontalDivider(modifier = Modifier.padding(vertical = 8.dp))
                TextButton(
                    onClick = { confirmRemoveDownload = true },
                    modifier = Modifier.padding(vertical = 4.dp),
                ) { Text("Remove download") }
                Text(
                    "Frees space on this phone. The book stays in your library.",
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }

            if (offerLibraryDelete) {
                HorizontalDivider(modifier = Modifier.padding(vertical = 8.dp))
                TextButton(
                    onClick = { confirmDelete = true },
                    modifier = Modifier.padding(vertical = 4.dp),
                ) { Text("Delete from library…", color = MaterialTheme.colorScheme.error) }
            }
        }
    }
}

@Composable
private fun WatchRow(label: String, detail: String, checked: Boolean, enabled: Boolean, onToggle: () -> Unit) {
    Row(
        modifier = Modifier.fillMaxWidth().padding(vertical = 8.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Column(modifier = Modifier.weight(1f)) {
            Text(label, color = MaterialTheme.colorScheme.onSurface)
            if (detail.isNotEmpty()) {
                Text(
                    detail,
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
            }
        }
        Switch(checked = checked, onCheckedChange = { onToggle() }, enabled = enabled)
    }
}
