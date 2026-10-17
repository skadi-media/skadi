package com.skadi.app

import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.aspectRatio
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.grid.GridCells
import androidx.compose.foundation.lazy.grid.LazyVerticalGrid
import androidx.compose.foundation.lazy.grid.items
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Download
import androidx.compose.material.icons.filled.DownloadDone
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.FilterChip
import androidx.compose.material3.FilterChipDefaults
import androidx.compose.material3.Icon
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.produceState
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import coil.compose.AsyncImage
import com.skadi.core.Episode
import com.skadi.core.OfflineVideo
import com.skadi.core.OfflineVideoStore
import com.skadi.core.Series
import com.skadi.core.VideoDownloadRequest
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

/*
 * Films and episodes on the phone (SKADI-T-0637): the controls on the film,
 * episode and series pages, and the Downloaded view of the Movies and TV tabs.
 * The rules are the books' rules: app storage, no budget, download on any
 * connection (the text says Wi-Fi), remove to free the space.
 */

@Composable
fun rememberVideoStore(): OfflineVideoStore {
    val context = LocalContext.current
    return remember { OfflineVideoStore(context.filesDir) }
}

/** Every complete video download, re-read when one finishes or is removed. */
@Composable
fun offlineVideos(): List<OfflineVideo> {
    val store = rememberVideoStore()
    val changes by DownloadCenter.videoChanges.collectAsState()
    val list by produceState(initialValue = emptyList<OfflineVideo>(), changes) {
        value = withContext(Dispatchers.IO) { store.list() }
    }
    return list
}

/** Remove a download, off the main thread, and tell the lists. */
fun removeVideo(store: OfflineVideoStore, key: String, scope: kotlinx.coroutines.CoroutineScope) {
    scope.launch {
        withContext(Dispatchers.IO) { store.delete(key) }
        DownloadCenter.videosChanged()
    }
}

/** The `file://` URI of a downloaded video, or null when it is not on the phone. */
fun localVideoUri(store: OfflineVideoStore, key: String): String? =
    store.videoFile(key).takeIf { store.isDownloaded(key) }?.toURI()?.toString()

@Composable
private fun ConfirmRemove(title: String, size: Long, onConfirm: () -> Unit, onDismiss: () -> Unit) {
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text("Remove download?") },
        text = { Text("Frees ${formatBytes(size)}. “$title” stays in your library and can be downloaded again.") },
        confirmButton = {
            TextButton(onClick = onConfirm) { Text("Remove", color = MaterialTheme.colorScheme.error) }
        },
        dismissButton = { TextButton(onClick = onDismiss) { Text("Cancel") } },
    )
}

/**
 * Download, progress with Cancel, or "On this phone" with Remove, for one film
 * or episode. Sits under Play on its page.
 */
@Composable
fun VideoDownloadAction(req: VideoDownloadRequest) {
    val context = LocalContext.current
    val store = rememberVideoStore()
    val scope = rememberCoroutineScope()
    val key = req.key
    val downloading by DownloadCenter.progress.collectAsState()
    val errors by DownloadCenter.errors.collectAsState()
    val changes by DownloadCenter.videoChanges.collectAsState()
    val meta by produceState<OfflineVideo?>(initialValue = null, key, changes) {
        value = withContext(Dispatchers.IO) { store.meta(key) }
    }
    var confirm by remember { mutableStateOf(false) }
    val p = downloading[key]
    val err = errors[key]
    val m = meta

    if (confirm && m != null) {
        ConfirmRemove(m.title, m.sizeBytes, onConfirm = { confirm = false; removeVideo(store, key, scope) }, onDismiss = { confirm = false })
    }

    Column(modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 4.dp)) {
        when {
            m != null -> Row(verticalAlignment = Alignment.CenterVertically) {
                Icon(Icons.Filled.DownloadDone, contentDescription = null, tint = MaterialTheme.colorScheme.primary)
                Text(
                    "On this phone · ${formatBytes(m.sizeBytes)}",
                    style = MaterialTheme.typography.bodyMedium,
                    modifier = Modifier.weight(1f).padding(start = 8.dp),
                )
                TextButton(onClick = { confirm = true }) {
                    Text("Remove", color = MaterialTheme.colorScheme.error)
                }
            }
            p != null -> {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Text(
                        if (p > 0f) "Downloading · ${(p * 100).toInt()}%" else "Waiting to download…",
                        style = MaterialTheme.typography.bodyMedium,
                        modifier = Modifier.weight(1f),
                    )
                    TextButton(onClick = { DownloadCenter.cancelVideo(context, key) }) { Text("Cancel") }
                }
                LinearProgressIndicator(progress = { p }, modifier = Modifier.fillMaxWidth())
            }
            else -> {
                OutlinedButton(
                    onClick = { DownloadCenter.startVideo(context, req) },
                    modifier = Modifier.fillMaxWidth(),
                ) {
                    Icon(Icons.Filled.Download, contentDescription = null, modifier = Modifier.padding(end = 8.dp))
                    Text(if (err != null) "Try the download again" else "Download to watch offline")
                }
                if (err != null) {
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        Text(
                            err,
                            color = MaterialTheme.colorScheme.error,
                            style = MaterialTheme.typography.bodySmall,
                            modifier = Modifier.weight(1f),
                        )
                        // A failed download may have left a part file: Cancel frees it.
                        TextButton(onClick = { DownloadCenter.cancelVideo(context, key) }) { Text("Cancel") }
                    }
                }
            }
        }
    }
}

/**
 * Queue a season, or the whole show, as one action (SKADI-T-0637): how people
 * pack for a trip. Each episode is its own download, so one failure does not
 * stop the rest and each can be cancelled or removed alone.
 */
@Composable
fun SeriesDownloadRow(series: Series, season: Int) {
    val context = LocalContext.current
    val store = rememberVideoStore()
    val scope = rememberCoroutineScope()
    val videos = offlineVideos()
    val downloading by DownloadCenter.progress.collectAsState()
    var confirm by remember { mutableStateOf(false) }
    val playable = series.episodes.filter { it.playable }
    val inSeason = playable.filter { it.season == season }
    val onPhone = videos.filter { !it.isMovie && it.parentId == series.id }
    val seasonOnPhone = onPhone.filter { it.season == season }
    val req = { e: Episode -> VideoDownloadRequest.episode(series, e) }
    val missing = { eps: List<Episode> ->
        eps.filter { e -> onPhone.none { it.itemId == e.id } && downloading[req(e).key] == null }
    }
    val seasonMissing = missing(inSeason)
    val allMissing = missing(playable)
    val label = if (season == 0) "Specials" else "season $season"

    if (confirm) {
        AlertDialog(
            onDismissRequest = { confirm = false },
            title = { Text("Remove $label from this phone?") },
            text = { Text("Frees ${formatBytes(seasonOnPhone.sumOf { it.sizeBytes })}. The episodes stay in your library.") },
            confirmButton = {
                TextButton(onClick = {
                    confirm = false
                    scope.launch {
                        withContext(Dispatchers.IO) { seasonOnPhone.forEach { store.delete(it.key) } }
                        DownloadCenter.videosChanged()
                    }
                }) { Text("Remove", color = MaterialTheme.colorScheme.error) }
            },
            dismissButton = { TextButton(onClick = { confirm = false }) { Text("Cancel") } },
        )
    }

    Column(modifier = Modifier.padding(horizontal = 8.dp)) {
        if (seasonOnPhone.isNotEmpty()) {
            Text(
                "${seasonOnPhone.size} of ${inSeason.size} on this phone · ${formatBytes(seasonOnPhone.sumOf { it.sizeBytes })}",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.padding(horizontal = 8.dp),
            )
        }
        Row(horizontalArrangement = Arrangement.spacedBy(4.dp)) {
            if (seasonMissing.isNotEmpty()) {
                TextButton(onClick = { seasonMissing.forEach { DownloadCenter.startVideo(context, req(it)) } }) {
                    Text("Download $label (${seasonMissing.size})")
                }
            }
            if (allMissing.size > seasonMissing.size) {
                TextButton(onClick = { allMissing.forEach { DownloadCenter.startVideo(context, req(it)) } }) {
                    Text("Download all (${allMissing.size})")
                }
            }
            if (seasonOnPhone.isNotEmpty()) {
                TextButton(onClick = { confirm = true }) {
                    Text("Remove $label", color = MaterialTheme.colorScheme.error)
                }
            }
        }
    }
}

/** A small mark on an episode row: done, or the percent while it downloads. */
@Composable
fun VideoDownloadMark(key: String, onPhone: Boolean) {
    val downloading by DownloadCenter.progress.collectAsState()
    val p = downloading[key]
    when {
        onPhone -> Icon(
            Icons.Filled.DownloadDone,
            contentDescription = "On this phone",
            tint = MaterialTheme.colorScheme.primary,
            modifier = Modifier.padding(end = 4.dp).size(18.dp),
        )
        p != null -> Text(
            "${(p * 100).toInt()}%",
            style = MaterialTheme.typography.labelMedium,
            color = MaterialTheme.colorScheme.primary,
            modifier = Modifier.padding(end = 4.dp),
        )
    }
}

/** All / Downloaded, for the Movies and TV tabs; shown when anything is on the phone. */
@Composable
fun DownloadedToggle(downloaded: Boolean, onChange: (Boolean) -> Unit, offline: Boolean) {
    Column {
        Row(
            modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp),
            horizontalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            listOf(false to "All", true to "Downloaded").forEach { (v, label) ->
                FilterChip(
                    selected = downloaded == v,
                    onClick = { onChange(v) },
                    label = { Text(label) },
                    colors = FilterChipDefaults.filterChipColors(
                        selectedContainerColor = MaterialTheme.colorScheme.secondaryContainer,
                        selectedLabelColor = MaterialTheme.colorScheme.onSecondaryContainer,
                    ),
                )
            }
        }
        if (offline) {
            Text(
                "Offline — showing what is on this phone",
                color = MaterialTheme.colorScheme.tertiary,
                style = MaterialTheme.typography.bodySmall,
                modifier = Modifier.padding(horizontal = 16.dp, vertical = 4.dp),
            )
        }
    }
}

/**
 * The downloaded films, as a poster grid. Tap plays from the phone; long press
 * removes. Works with no network: everything here comes from the store.
 */
@OptIn(ExperimentalFoundationApi::class)
@Composable
fun DownloadedMovies(videos: List<OfflineVideo>, query: String, onPlay: (OfflineVideo) -> Unit) {
    val store = rememberVideoStore()
    val scope = rememberCoroutineScope()
    var confirm by remember { mutableStateOf<OfflineVideo?>(null) }
    val q = query.trim().lowercase()
    val films = videos.filter { it.isMovie && (q.isEmpty() || it.title.lowercase().contains(q)) }
    confirm?.let { v ->
        ConfirmRemove(v.title, v.sizeBytes, onConfirm = { confirm = null; removeVideo(store, v.key, scope) }, onDismiss = { confirm = null })
    }
    if (films.isEmpty()) {
        EmptyState(
            if (q.isNotEmpty()) "No downloaded films match “$query”."
            else "No films on this phone. Open a film and tap Download, on Wi-Fi, to watch it offline.",
        )
        return
    }
    LazyVerticalGrid(
        columns = GridCells.Adaptive(100.dp),
        contentPadding = PaddingValues(16.dp),
        horizontalArrangement = Arrangement.spacedBy(10.dp),
        verticalArrangement = Arrangement.spacedBy(14.dp),
        modifier = Modifier.fillMaxSize(),
    ) {
        items(films, key = { it.key }) { v ->
            Column(modifier = Modifier.combinedClickable(onClick = { onPlay(v) }, onLongClick = { confirm = v })) {
                VideoArt(store, v, Modifier.fillMaxWidth())
                Text(v.title, style = MaterialTheme.typography.bodySmall, maxLines = 1, overflow = TextOverflow.Ellipsis, modifier = Modifier.padding(top = 6.dp))
                Text(
                    listOfNotNull(v.subtitle, formatBytes(v.sizeBytes)).joinToString(" · "),
                    style = MaterialTheme.typography.labelSmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        }
    }
}

/** The downloaded episodes, grouped by show, in broadcast order. */
@OptIn(ExperimentalFoundationApi::class)
@Composable
fun DownloadedEpisodes(videos: List<OfflineVideo>, query: String, onPlay: (OfflineVideo) -> Unit) {
    val store = rememberVideoStore()
    val scope = rememberCoroutineScope()
    var confirm by remember { mutableStateOf<OfflineVideo?>(null) }
    val q = query.trim().lowercase()
    val eps = videos.filter { !it.isMovie && (q.isEmpty() || it.title.lowercase().contains(q)) }
    confirm?.let { v ->
        ConfirmRemove("${v.title} · ${v.subtitle.orEmpty()}", v.sizeBytes, onConfirm = { confirm = null; removeVideo(store, v.key, scope) }, onDismiss = { confirm = null })
    }
    if (eps.isEmpty()) {
        EmptyState(
            if (q.isNotEmpty()) "No downloaded shows match “$query”."
            else "No episodes on this phone. Open a show and download an episode or a season, on Wi-Fi, to watch offline.",
        )
        return
    }
    val shows = eps.groupBy { it.parentId }.values.sortedBy { it.first().title.lowercase() }
    LazyColumn(contentPadding = PaddingValues(bottom = 16.dp), modifier = Modifier.fillMaxSize()) {
        shows.forEach { show ->
            val first = show.first()
            item(key = "h:${first.parentId}") {
                Row(
                    modifier = Modifier.fillMaxWidth().padding(start = 16.dp, end = 16.dp, top = 12.dp, bottom = 4.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    VideoArt(store, first, Modifier.size(width = 40.dp, height = 60.dp))
                    Column(modifier = Modifier.padding(start = 12.dp)) {
                        Text(first.title, style = MaterialTheme.typography.titleMedium, maxLines = 1, overflow = TextOverflow.Ellipsis)
                        Text(
                            "${show.size} episode${if (show.size == 1) "" else "s"} · ${formatBytes(show.sumOf { it.sizeBytes })}",
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                    }
                }
            }
            items(show, key = { it.key }) { v ->
                Row(
                    modifier = Modifier
                        .fillMaxWidth()
                        .combinedClickable(onClick = { onPlay(v) }, onLongClick = { confirm = v })
                        .padding(horizontal = 16.dp, vertical = 10.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    Text(
                        v.subtitle ?: "Episode ${v.number ?: ""}",
                        style = MaterialTheme.typography.bodyLarge,
                        maxLines = 2,
                        overflow = TextOverflow.Ellipsis,
                        modifier = Modifier.weight(1f),
                    )
                    Text(
                        formatBytes(v.sizeBytes),
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
            }
        }
    }
}

/** The saved poster, or the server's when it was not saved, or a blank slot. */
@Composable
private fun VideoArt(store: OfflineVideoStore, v: OfflineVideo, modifier: Modifier) {
    val shape = RoundedCornerShape(6.dp)
    val model: Any? = store.posterFile(v.key).takeIf { it.isFile } ?: v.posterUrl
    if (model != null) {
        AsyncImage(
            model = model,
            contentDescription = v.title,
            contentScale = ContentScale.Crop,
            modifier = modifier.aspectRatio(2f / 3f).clip(shape),
        )
    } else {
        ArtPlaceholder(modifier = modifier, shape = shape)
    }
}
