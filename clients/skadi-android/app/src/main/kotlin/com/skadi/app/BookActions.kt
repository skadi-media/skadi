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
fun BookActionsSheet(book: Book, api: SkadiApi, onDismiss: () -> Unit, onDeleted: () -> Unit) {
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
    var deleteFiles by remember { mutableStateOf(true) }

    LaunchedEffect(book.id) {
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

            if (authorName != null) {
                WatchRow(
                    label = "Watch author",
                    detail = authorName + if (resolved && authorAsin == null) " — not on Audible" else "",
                    checked = watchingAuthor,
                    enabled = authorAsin != null && !busy,
                    onToggle = { toggleWatch("author", authorAsin, watchingAuthor) { watchingAuthor = it } },
                )
            }
            if (seriesName != null) {
                WatchRow(
                    label = "Watch series",
                    detail = seriesName + if (resolved && seriesAsin == null) " — no series match" else "",
                    checked = watchingSeries,
                    enabled = seriesAsin != null && !busy,
                    onToggle = { toggleWatch("series", seriesAsin, watchingSeries) { watchingSeries = it } },
                )
            }

            HorizontalDivider(modifier = Modifier.padding(vertical = 8.dp))
            TextButton(
                onClick = { confirmDelete = true },
                modifier = Modifier.padding(vertical = 4.dp),
            ) { Text("Delete from library…", color = MaterialTheme.colorScheme.error) }
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
