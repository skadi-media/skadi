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
import com.skadi.core.OfflineStore
import com.skadi.core.OfflineVideoStore
import com.skadi.core.StorageUse
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

/** One download on the phone, book or video, as the Storage list shows it. */
private data class Held(val key: String, val title: String, val sub: String, val size: Long, val remove: () -> Unit)

/**
 * On-device storage management (SKADI-I-0052, Track A): the downloads taking up
 * space on THIS phone — total footprint, free space, and per-item evict. Books
 * and, since SKADI-T-0637, films and episodes, under the same rules: no budget,
 * the biggest first, Remove frees the space. Removing only drops the local copy;
 * the item stays in the server library and can be downloaded again.
 */
@Composable
fun StorageScreen(onBack: () -> Unit) {
    val context = LocalContext.current
    val store = remember { OfflineStore(context.filesDir) }
    val videoStore = remember { OfflineVideoStore(context.filesDir) }
    val scope = rememberCoroutineScope()

    var shelf by remember { mutableStateOf<List<Held>>(emptyList()) }
    var use by remember { mutableStateOf(StorageUse(0, 0, 0)) }
    var loading by remember { mutableStateOf(true) }
    var confirm by remember { mutableStateOf<Held?>(null) }

    fun reload() {
        scope.launch {
            withContext(Dispatchers.IO) {
                val books = store.list()
                val videos = videoStore.list()
                use = StorageUse.of(books, videos, videoStore.partialBytesTotal())
                shelf = (
                    books.map { b ->
                        Held(
                            key = "b:${b.fileId}",
                            title = b.title,
                            sub = listOfNotNull(
                                b.authors.joinToString(", ").ifEmpty { null },
                                b.seriesName?.let { s -> s + (b.seriesPosition?.let { " #$it" } ?: "") },
                            ).joinToString(" · "),
                            size = b.sizeBytes,
                        ) { store.delete(b.fileId) }
                    } + videos.map { v ->
                        Held(
                            key = "v:${v.key}",
                            title = v.title,
                            sub = listOfNotNull(if (v.isMovie) "Film" else null, v.subtitle).joinToString(" · "),
                            size = v.sizeBytes,
                        ) { videoStore.delete(v.key) }
                    }
                    ).sortedByDescending { it.size }
            }
            loading = false
        }
    }
    LaunchedEffect(Unit) { reload() }

    val free = remember(loading, use) { context.filesDir.usableSpace }

    confirm?.let { item ->
        AlertDialog(
            onDismissRequest = { confirm = null },
            title = { Text("Remove download?") },
            text = {
                Text(
                    "Frees ${formatBytes(item.size)}. “${item.title}” stays in your " +
                        "library and can be downloaded again later.",
                )
            },
            confirmButton = {
                TextButton(onClick = {
                    confirm = null
                    scope.launch {
                        withContext(Dispatchers.IO) { item.remove() }
                        DownloadCenter.videosChanged()
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
                        listOfNotNull(
                            "Books ${formatBytes(use.books)}",
                            "video ${formatBytes(use.videos)}",
                            use.partial.takeIf { it > 0 }?.let { "unfinished ${formatBytes(it)}" },
                        ).joinToString(" · "),
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                    Text(
                        "${formatBytes(free)} free on device",
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
                Text(
                    formatBytes(use.total),
                    style = MaterialTheme.typography.headlineSmall,
                    color = MaterialTheme.colorScheme.primary,
                )
            }
        }

        when {
            loading -> Loading()
            shelf.isEmpty() -> EmptyState("Nothing downloaded yet. Download books, films or episodes to keep them offline.")
            else -> LazyColumn(modifier = Modifier.fillMaxSize()) {
                items(shelf, key = { it.key }) { item ->
                    Row(
                        modifier = Modifier.fillMaxWidth().padding(start = 16.dp, top = 10.dp, bottom = 10.dp),
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        Column(modifier = Modifier.weight(1f)) {
                            Text(
                                item.title,
                                color = MaterialTheme.colorScheme.onSurface,
                                maxLines = 1,
                                overflow = TextOverflow.Ellipsis,
                            )
                            if (item.sub.isNotEmpty()) {
                                Text(
                                    item.sub,
                                    style = MaterialTheme.typography.bodySmall,
                                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                                    maxLines = 1,
                                    overflow = TextOverflow.Ellipsis,
                                )
                            }
                        }
                        Text(
                            formatBytes(item.size),
                            style = MaterialTheme.typography.bodyMedium,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                            modifier = Modifier.padding(horizontal = 12.dp),
                        )
                        TextButton(onClick = { confirm = item }) {
                            Text("Remove", color = MaterialTheme.colorScheme.error)
                        }
                    }
                }
            }
        }
    }
}
