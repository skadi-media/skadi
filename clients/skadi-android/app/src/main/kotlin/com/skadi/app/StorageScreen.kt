package com.skadi.app

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Card
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.MaterialTheme
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
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import com.skadi.core.OfflineBook
import com.skadi.core.OfflineStore
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

/** Human-readable byte size — "734 MB", "1.2 GB". Binary units (matches OS). */
fun formatBytes(bytes: Long): String {
    if (bytes < 1024) return "$bytes B"
    val units = listOf("KB", "MB", "GB", "TB")
    var v = bytes.toDouble() / 1024
    var i = 0
    while (v >= 1024 && i < units.size - 1) { v /= 1024; i++ }
    return if (v >= 10) "%.0f %s".format(v, units[i]) else "%.1f %s".format(v, units[i])
}

/**
 * On-device storage management (SKADI-I-0052, Track A): the downloads taking up
 * space on THIS phone — total footprint, free space, and per-book evict. Evicting
 * only removes the local copy (`OfflineStore.delete`); the book stays in the server
 * library and can be re-downloaded. Server-side deletion is Track B.
 */
@Composable
fun StorageScreen(onBack: () -> Unit) {
    val context = LocalContext.current
    val store = remember { OfflineStore(context.filesDir) }
    val scope = rememberCoroutineScope()

    var shelf by remember { mutableStateOf<List<OfflineBook>>(emptyList()) }
    var loading by remember { mutableStateOf(true) }
    var confirm by remember { mutableStateOf<OfflineBook?>(null) }

    fun reload() {
        scope.launch {
            shelf = withContext(Dispatchers.IO) { store.list().sortedByDescending { it.sizeBytes } }
            loading = false
        }
    }
    LaunchedEffect(Unit) { reload() }

    val used = shelf.sumOf { it.sizeBytes }
    val free = remember(loading) { context.filesDir.usableSpace }

    confirm?.let { book ->
        AlertDialog(
            onDismissRequest = { confirm = null },
            title = { Text("Remove download?") },
            text = {
                Text(
                    "Frees ${formatBytes(book.sizeBytes)}. “${book.title}” stays in your " +
                        "library and can be downloaded again later.",
                )
            },
            confirmButton = {
                TextButton(onClick = {
                    val fid = book.fileId
                    confirm = null
                    scope.launch {
                        withContext(Dispatchers.IO) { store.delete(fid) }
                        reload()
                    }
                }) { Text("Remove", color = MaterialTheme.colorScheme.error) }
            },
            dismissButton = { TextButton(onClick = { confirm = null }) { Text("Cancel") } },
        )
    }

    Column(modifier = Modifier.fillMaxSize()) {
        SkadiTopBar(title = "Storage", onBack = onBack)

        // Footprint summary.
        Card(
            modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 8.dp),
            colors = CardDefaults.cardColors(containerColor = MaterialTheme.colorScheme.surfaceVariant),
        ) {
            Row(
                modifier = Modifier.fillMaxWidth().padding(16.dp),
                horizontalArrangement = Arrangement.SpaceBetween,
            ) {
                Column {
                    Text("${shelf.size} downloaded", color = MaterialTheme.colorScheme.onSurface)
                    Text(
                        "${formatBytes(free)} free on device",
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
                Text(
                    formatBytes(used),
                    style = MaterialTheme.typography.headlineSmall,
                    color = MaterialTheme.colorScheme.primary,
                )
            }
        }

        when {
            loading -> Loading()
            shelf.isEmpty() -> EmptyState("Nothing downloaded yet. Download books from the library to keep them offline.")
            else -> LazyColumn(modifier = Modifier.fillMaxSize()) {
                items(shelf, key = { it.fileId }) { book ->
                    Row(
                        modifier = Modifier.fillMaxWidth().padding(start = 16.dp, top = 10.dp, bottom = 10.dp),
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        Column(modifier = Modifier.weight(1f)) {
                            Text(
                                book.title,
                                color = MaterialTheme.colorScheme.onSurface,
                                maxLines = 1,
                                overflow = TextOverflow.Ellipsis,
                            )
                            val sub = listOfNotNull(
                                book.authors.joinToString(", ").ifEmpty { null },
                                book.seriesName?.let { s -> s + (book.seriesPosition?.let { " #$it" } ?: "") },
                            ).joinToString(" · ")
                            if (sub.isNotEmpty()) {
                                Text(
                                    sub,
                                    style = MaterialTheme.typography.bodySmall,
                                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                                    maxLines = 1,
                                    overflow = TextOverflow.Ellipsis,
                                )
                            }
                        }
                        Text(
                            formatBytes(book.sizeBytes),
                            style = MaterialTheme.typography.bodyMedium,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                            modifier = Modifier.padding(horizontal = 12.dp),
                        )
                        TextButton(onClick = { confirm = book }) {
                            Text("Remove", color = MaterialTheme.colorScheme.error)
                        }
                    }
                }
            }
        }
    }
}
