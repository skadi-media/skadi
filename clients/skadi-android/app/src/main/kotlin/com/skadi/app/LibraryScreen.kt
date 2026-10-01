package com.skadi.app

import kotlinx.coroutines.launch
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.aspectRatio
import androidx.compose.foundation.lazy.grid.items
import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.clickable
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.ChevronRight
import androidx.compose.material.icons.filled.Download
import androidx.compose.material.icons.filled.ExpandLess
import androidx.compose.material.icons.filled.ExpandMore
import androidx.compose.material.icons.filled.PlayArrow
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FilterChip
import androidx.compose.material3.FilterChipDefaults
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateListOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.StrokeCap
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import coil.compose.AsyncImage
import com.skadi.core.Book
import com.skadi.core.BookActionPolicy
import com.skadi.core.NameSorting
import com.skadi.core.OfflineBook
import com.skadi.core.OfflineStore
import com.skadi.core.SkadiApi
import com.skadi.core.groupBooksBySeries
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext

/**
 * Audiobook library + download manager (SKADI-T-0342; hardened in review pass
 * 2): downloads run in [DownloadCenter]'s process-lifetime scope (rotation/
 * backgrounding no longer kills them), the offline shelf is loaded off the
 * main thread once per refresh (not per recomposition), and books that exist
 * only on-device (removed server-side) still render when online.
 *
 * One tab of three since SKADI-T-0577; it no longer carries the app's header,
 * the Movies/TV doors, or its own search box. What is left above the list is
 * the one control this screen actually needs: the All / Downloaded / Series
 * filter.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun LibraryScreen(
    baseUrl: String,
    token: String,
    onPlay: (String) -> Unit,
) {
    val context = LocalContext.current
    val api = remember(baseUrl, token) { SkadiApi(baseUrl, token) }
    val store = remember { OfflineStore(context.filesDir) }
    // Downloads run in DownloadWorker (foreground service) now; it rebuilds its own
    // Downloader from Settings, so this screen no longer needs one (SKADI-T-0358).

    var books by remember { mutableStateOf<List<Book>>(emptyList()) }
    var online by remember { mutableStateOf(true) }
    var loading by remember { mutableStateOf(true) }
    var shelf by remember { mutableStateOf<List<OfflineBook>>(emptyList()) }
    val downloading by DownloadCenter.progress.collectAsState()
    val errors by DownloadCenter.errors.collectAsState()
    // Long-pressed book → the management action sheet (Track B). A delete bumps
    // reloadKey to re-fetch the library.
    var actionsFor by remember { mutableStateOf<Book?>(null) }
    // A device-only book has no library `Book` to hang the sheet on, so it gets
    // its own confirm (SKADI-T-0646) — these tiles previously had no long-press
    // at all, making a device-only download unremovable from this screen.
    var removeShelfOnly by remember { mutableStateOf<OfflineBook?>(null) }
    var reloadKey by remember { mutableStateOf(0) }
    val sheetScope = rememberCoroutineScope()
    var refreshing by remember { mutableStateOf(false) }
    // A tapped book opens its page; Listen / Download live there (SKADI-T-0586).
    var openBook by rememberSaveable { mutableStateOf<String?>(null) }
    // Explore (SKADI-T-0604): an author's or a series' whole body of work.
    var openAuthor by rememberSaveable { mutableStateOf<String?>(null) }
    var openSeries by rememberSaveable { mutableStateOf<String?>(null) }
    // Every library book, wanted ones included; `books` stays the on-disk slice.
    var allBooks by remember { mutableStateOf<List<Book>>(emptyList()) }
    var rollups by remember { mutableStateOf<List<com.skadi.core.SeriesRollup>>(emptyList()) }

    LaunchedEffect(reloadKey) {
        runCatching { api.listBooks() }
            .onSuccess { allBooks = it; books = it.filter { b -> b.importedFileId != null }; online = true }
            .onFailure { online = false }
        loading = false
        refreshing = false
        runCatching { api.listBookSeries() }.onSuccess { rollups = it }
    }
    // Reload the shelf when a download finishes (map shrinks) — on IO.
    LaunchedEffect(downloading.size) {
        shelf = withContext(Dispatchers.IO) { store.list() }
    }

    // Offline → default to the Downloaded view (the only thing that works on a
    // plane), so the library isn't a wall of dead Download buttons (SKADI-T-0350).
    var mode by rememberSaveable { mutableStateOf("all") } // all | downloaded | series | authors
    LaunchedEffect(online) { if (!online) mode = "downloaded" }
    var query by rememberSaveable { mutableStateOf("") }
    val expanded = remember { mutableStateListOf<String>() }

    // Long-press management sheet. The controller actions on it — watch
    // author/series, delete from library — stay operator-only (SKADI-T-0614),
    // but **removing a download is not a controller action**, so the sheet is no
    // longer gated on the operator role as a whole: that left a member unable to
    // reclaim space on their own phone (SKADI-T-0646).
    actionsFor?.let { b ->
        val fid = b.importedFileId
        val onDevice = fid?.takeIf { f -> shelf.any { it.fileId == f } }
        if (BookActionPolicy.sheetIsUseful(isAdmin(), onDevice != null)) {
            BookActionsSheet(
                book = b,
                api = api,
                downloadedFid = onDevice,
                // The rule lives in BookActionPolicy so it can be tested: the
                // admin path cannot be driven on an emulator (the API refuses a
                // second admin), which left the most important case — an
                // operator in the Downloaded view — unprovable from the UI.
                offerLibraryDelete = BookActionPolicy.offersLibraryDelete(isAdmin(), mode),
                offerWatchControls = BookActionPolicy.offersWatchControls(isAdmin()),
                onRemoveDownload = { f ->
                    sheetScope.launch {
                        withContext(Dispatchers.IO) { store.delete(f) }
                        reloadKey++
                    }
                },
                onDismiss = { actionsFor = null },
                onDeleted = { reloadKey++ },
            )
        }
    }

    removeShelfOnly?.let { b ->
        AlertDialog(
            onDismissRequest = { removeShelfOnly = null },
            title = { Text("Remove download?") },
            text = {
                Text(
                    "“${b.title}” is removed from this phone. It is not in your " +
                        "library, so this is the only copy on this device.",
                )
            },
            confirmButton = {
                TextButton(onClick = {
                    val fid = b.fileId
                    removeShelfOnly = null
                    sheetScope.launch {
                        withContext(Dispatchers.IO) { store.delete(fid) }
                        reloadKey++
                    }
                }) { Text("Remove", color = MaterialTheme.colorScheme.error) }
            },
            dismissButton = {
                TextButton(onClick = { removeShelfOnly = null }) { Text("Cancel") }
            },
        )
    }

    openBook?.let { id ->
        val b = allBooks.firstOrNull { it.id == id }
        val fid = b?.importedFileId
        if (b != null) {
            androidx.activity.compose.BackHandler { openBook = null }
            BookDetailScreen(
                book = b,
                api = api,
                downloaded = fid != null && shelf.any { it.fileId == fid },
                downloading = fid?.let { downloading[it] },
                onPlay = { onPlay(it) },
                onDownload = { DownloadCenter.start(context, b, it) },
                onBack = { openBook = null },
                onNarrator = { n -> openBook = null; openAuthor = null; openSeries = null; mode = "all"; query = n },
            )
            return
        }
    }
    openAuthor?.let { name ->
        androidx.activity.compose.BackHandler { openAuthor = null }
        AuthorScreen(name = name, api = api, library = allBooks, onOpenBook = { openBook = it }, onBack = { openAuthor = null; reloadKey++ })
        return
    }
    openSeries?.let { name ->
        androidx.activity.compose.BackHandler { openSeries = null }
        val asin = rollups.firstOrNull { nameKey(it.name) == nameKey(name) }?.seriesAsin
        SeriesExploreScreen(name = name, seriesAsin = asin, api = api, library = allBooks, onOpenBook = { openBook = it }, onBack = { openSeries = null; reloadKey++ })
        return
    }

    Column(modifier = Modifier.fillMaxSize()) {
        SkadiTopBar(
            title = "Audiobooks",
            query = query,
            onQuery = { query = it },
            placeholder = "Search title or author",
        )
        // Storage, Change password and Unpair used to hang off an overflow
        // here (SKADI-T-0627). None of them are about books; they are account
        // actions, and they now live on Home's account menu where every tab
        // can reach them.

        // All / Downloaded / Series. Chips rather than the segmented control:
        // same job, half the visual weight, and the same control the TV screen
        // uses for seasons, so the app has one way of doing "pick one".
        val modes = listOf("all" to "All", "downloaded" to "Downloaded", "series" to "Series", "authors" to "Authors")
        Row(
            modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp),
            horizontalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            modes.forEach { (key, label) ->
                FilterChip(
                    selected = mode == key,
                    onClick = { mode = key },
                    label = { Text(label) },
                    colors = FilterChipDefaults.filterChipColors(
                        selectedContainerColor = MaterialTheme.colorScheme.secondaryContainer,
                        selectedLabelColor = MaterialTheme.colorScheme.onSecondaryContainer,
                    ),
                )
            }
        }
        if (!online) {
            Text(
                "Offline — showing books on this device",
                color = MaterialTheme.colorScheme.tertiary,
                style = MaterialTheme.typography.bodySmall,
                modifier = Modifier.padding(horizontal = 16.dp, vertical = 4.dp),
            )
        }
        val onlineFids = books.mapNotNull { it.importedFileId }.toSet()
        val shelfOnly = shelf.filter { it.fileId !in onlineFids }

        // Per-book renderer shared by both modes.
        @Composable
        fun bookItem(b: Book) {
            val fid = b.importedFileId ?: return
            val prog = downloading[fid]
            val err = errors[fid]
            val isDown = shelf.any { it.fileId == fid }
            val seriesSub = if (mode == "all") {
                b.series?.let { s -> s.name + (s.position?.let { " #$it" } ?: "") }
            } else {
                b.series?.position?.let { "Book $it" } // inside a group: just the number
            }
            BookRow(
                title = b.title,
                subtitle = listOfNotNull(b.authors.joinToString(", ").ifEmpty { null }, seriesSub)
                    .joinToString(" · "),
                coverModel = b.coverUrl?.takeIf { it.isNotEmpty() },
                onClick = { openBook = b.id },
                trailing = {
                    when {
                        isDown -> Icon(
                            Icons.Filled.PlayArrow,
                            contentDescription = "Play",
                            tint = MaterialTheme.colorScheme.primary,
                        )
                        // Percent in the same slot the buttons occupy, at the
                        // weight of a button label rather than a caption — the
                        // most dynamic state in the app used to be the quietest
                        // thing on screen (SKADI-T-0573).
                        prog != null -> Text(
                            "${(prog * 100).toInt()}%",
                            color = MaterialTheme.colorScheme.primary,
                            style = MaterialTheme.typography.titleMedium,
                        )
                        else -> IconButton(onClick = { DownloadCenter.start(context, b, fid) }) {
                            Icon(
                                Icons.Filled.Download,
                                contentDescription = "Download for offline listening",
                                tint = MaterialTheme.colorScheme.onSurfaceVariant,
                            )
                        }
                    }
                },
                progress = prog,
                error = if (prog == null) err else null,
                onLongPress = { actionsFor = b },
            )
        }

        // Filter by search query; in Downloaded mode, only on-device books.
        val q = query.trim().lowercase()
        val matches = { title: String, authors: List<String> ->
            q.isEmpty() || title.lowercase().contains(q) ||
                authors.any { it.lowercase().contains(q) }
        }
        val downFids = shelf.map { it.fileId }.toSet()
        val visibleBooks = books.filter { b ->
            (matches(b.title, b.authors) || b.narrators.any { it.lowercase().contains(q) }) &&
                (mode != "downloaded" || b.importedFileId in downFids)
        }
        // Authors mode is a rollup over the whole library (wanted included):
        // name → owned / wanted counts, tap → the author's body of work.
        if (mode == "authors") {
            val authors = allBooks.flatMap { b -> b.authors.map { it to b } }
                .groupBy({ nameKey(it.first) }, { it })
                .values
                .map { pairs -> Triple(pairs.first().first, pairs.count { it.second.importedFileId != null }, pairs.count { it.second.importedFileId == null }) }
                .filter { (n, _, _) -> q.isEmpty() || n.lowercase().contains(q) }
                // NameSorting, not the title sort: the rail below takes its
                // letters from the same object, so the two cannot disagree.
                // This list used to sort on a plain lowercase name while the
                // rail lettered with the article-stripping `sortLetter`, which
                // filed "An Na" under A and labelled it N (SKADI-T-0648 F2).
                .sortedBy { NameSorting.key(it.first) }
            if (authors.isEmpty()) { EmptyState(if (q.isNotEmpty()) "No authors match “$query”." else "No audiobooks in the library."); return@Column }
            // Same row shape as the TV list, with the rail (SKADI-T-0609): an
            // author's first cover as the thumbnail, name, owned/wanted line.
            val covers = remember(allBooks) {
                allBooks.flatMap { b -> b.authors.map { nameKey(it) to b } }
                    .groupBy({ it.first }, { it.second })
                    .mapValues { (_, bs) -> bs.firstNotNullOfOrNull { it.coverUrl?.takeIf { c -> c.isNotEmpty() } } }
            }
            val listState = androidx.compose.foundation.lazy.rememberLazyListState()
            val scope = rememberCoroutineScope()
            Row(modifier = Modifier.fillMaxSize()) {
                LazyColumn(
                    state = listState,
                    modifier = Modifier.weight(1f),
                    contentPadding = PaddingValues(top = 4.dp, bottom = 16.dp),
                ) {
                    items(authors, key = { it.first }) { (n, owned, wanted) ->
                        CompactRow(
                            cover = covers[nameKey(n)],
                            title = n,
                            meta = listOfNotNull("$owned owned", wanted.takeIf { it > 0 }?.let { "$it wanted" }).joinToString(" · "),
                            onClick = { openAuthor = n },
                        ) {
                            Icon(Icons.Filled.ChevronRight, contentDescription = "Open", tint = MaterialTheme.colorScheme.onSurfaceVariant)
                        }
                    }
                }
                AlphabetRail(
                    letters = remember(authors) { authors.map { NameSorting.letter(it.first) } },
                    onJump = { i -> scope.launch { listState.scrollToItem(i) } },
                )
            }
            return@Column
        }
        val visibleShelfOnly = shelfOnly.filter { matches(it.title, it.authors) }

        // Empty / loading states (SKADI-T-0350).
        if (loading && books.isEmpty() && shelf.isEmpty()) {
            Loading()
            return@Column
        }
        if (visibleBooks.isEmpty() && visibleShelfOnly.isEmpty()) {
            EmptyState(
                when {
                    q.isNotEmpty() -> "No books match “$query”."
                    mode == "downloaded" -> "Nothing downloaded yet. Browse All and tap the download icon while on Wi-Fi to take books offline."
                    !online -> "Offline and nothing downloaded yet. Connect to your network to browse and download."
                    else -> "No audiobooks in the library."
                },
            )
            return@Column
        }

Refreshable(refreshing = refreshing, onRefresh = { refreshing = true; reloadKey++ }) {
if (mode == "series") {
        // Series: a flat entry list (header / book / label) so the alphabet
        // rail can jump to a series header (SKADI-T-0609). Headers use the
        // compact TV-row shape with the first book's cover; books under an
        // open header keep the full row with their download state.
        val (groups, standalone) = remember(visibleBooks) { groupBooksBySeries(visibleBooks) }
        val entries = remember(groups, standalone, visibleShelfOnly, expanded.toList()) {
            buildList<Pair<Char, Any>> {
                visibleShelfOnly.forEach { add(sortLetter(it.title) to it) }
                for (g in groups) {
                    val l = sortLetter(g.name)
                    add(l to g)
                    if (g.seriesId in expanded) g.books.forEach { add(l to ("book" to it)) }
                }
                if (standalone.isNotEmpty()) {
                    add('#' to "Standalone")
                    standalone.forEach { add(sortLetter(it.title) to ("book" to it)) }
                }
            }
        }
        val listState = androidx.compose.foundation.lazy.rememberLazyListState()
        val scope = rememberCoroutineScope()
        Row(modifier = Modifier.fillMaxSize()) {
            LazyColumn(
                state = listState,
                modifier = Modifier.weight(1f),
                contentPadding = PaddingValues(top = 4.dp, bottom = 16.dp),
            ) {
                items(entries.size, key = { i ->
                    when (val e = entries[i].second) {
                        is OfflineBook -> "shelf-" + e.fileId
                        is com.skadi.core.SeriesGroup -> "series-" + e.seriesId
                        is String -> "label-$e"
                        else -> "book-" + ((e as Pair<*, *>).second as Book).id
                    }
                }) { i ->
                    when (val e = entries[i].second) {
                        is OfflineBook -> BookRow(
                            title = e.title,
                            subtitle = e.authors.joinToString(", "),
                            coverModel = store.coverFile(e.fileId).takeIf { it.isFile },
                            onClick = { onPlay(e.fileId) },
                            trailing = { Icon(Icons.Filled.PlayArrow, contentDescription = "Play", tint = MaterialTheme.colorScheme.primary) },
                        )
                        is com.skadi.core.SeriesGroup -> {
                            val isOpen = e.seriesId in expanded
                            val roll = rollups.firstOrNull { nameKey(it.name) == nameKey(e.name) }
                            val total = roll?.total?.takeIf { it > 0 }
                            CompactRow(
                                cover = e.books.firstNotNullOfOrNull { it.coverUrl?.takeIf { c -> c.isNotEmpty() } },
                                title = e.name,
                                meta = if (total != null && total > e.books.size) "${e.books.size} of $total books" else "${e.books.size} book${if (e.books.size == 1) "" else "s"}",
                                onClick = { if (isOpen) expanded.remove(e.seriesId) else expanded.add(e.seriesId) },
                            ) {
                                IconButton(onClick = { openSeries = e.name }) {
                                    Icon(Icons.Filled.ChevronRight, contentDescription = "Explore series", tint = MaterialTheme.colorScheme.onSurfaceVariant)
                                }
                                Icon(
                                    if (isOpen) Icons.Filled.ExpandLess else Icons.Filled.ExpandMore,
                                    contentDescription = if (isOpen) "Collapse" else "Expand",
                                    tint = MaterialTheme.colorScheme.onSurfaceVariant,
                                )
                            }
                        }
                        is String -> SectionLabel(e)
                        else -> bookItem((e as Pair<*, *>).second as Book)
                    }
                }
            }
            AlphabetRail(
                letters = remember(entries) { entries.map { it.first } },
                onJump = { i -> scope.launch { listState.scrollToItem(i) } },
            )
        }
        } else {
            // The wall (SKADI-T-0609): square covers on the same grid, spacing and
            // rail as the movie wall, so the three media tabs read as one app.
            val tiles = remember(visibleBooks, visibleShelfOnly, shelf, downloading) {
                (visibleBooks.map { b ->
                    val fid = b.importedFileId
                    BookTileData(
                        key = b.id, title = b.title,
                        author = b.authors.firstOrNull().orEmpty(),
                        cover = b.coverUrl?.takeIf { it.isNotEmpty() },
                        onDevice = fid != null && shelf.any { it.fileId == fid },
                        progress = fid?.let { downloading[it] },
                        open = { openBook = b.id }, more = { actionsFor = b },
                    )
                } + visibleShelfOnly.map { b ->
                    BookTileData(
                        key = "shelf-" + b.fileId, title = b.title,
                        author = b.authors.firstOrNull().orEmpty(),
                        cover = store.coverFile(b.fileId).takeIf { it.isFile },
                        onDevice = true, progress = null,
                        open = { onPlay(b.fileId) },
                        more = { removeShelfOnly = b },
                    )
                }).sortedBy { sortKey(it.title) }
            }
            BookWall(tiles)
        }
}
    }
}

@Composable
private fun SectionLabel(text: String) {
    Text(
        text,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
        style = MaterialTheme.typography.labelLarge,
        modifier = Modifier.padding(start = 16.dp, top = 16.dp, bottom = 4.dp),
    )
}

/** A tappable series header row: name, count, and a chevron that flips. */
@Composable
private fun SeriesHeader(
    name: String,
    count: Int,
    /** Books the series has in total (Audible), when known: "3 of 9". */
    total: Int? = null,
    open: Boolean,
    onToggle: () -> Unit,
    onExplore: (() -> Unit)? = null,
) {
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .clickable(onClick = onToggle)
            .padding(start = 16.dp, end = 4.dp, top = 12.dp, bottom = 12.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Column(modifier = Modifier.weight(1f)) {
            Text(name, style = MaterialTheme.typography.titleMedium, maxLines = 1, overflow = TextOverflow.Ellipsis)
            Text(
                if (total != null && total > count) "$count of $total books" else "$count book${if (count == 1) "" else "s"}",
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                style = MaterialTheme.typography.bodySmall,
            )
        }
        if (onExplore != null) {
            IconButton(onClick = onExplore) {
                Icon(Icons.Filled.ChevronRight, contentDescription = "Explore series", tint = MaterialTheme.colorScheme.onSurfaceVariant)
            }
        }
        Icon(
            if (open) Icons.Filled.ExpandLess else Icons.Filled.ExpandMore,
            contentDescription = if (open) "Collapse" else "Expand",
            tint = MaterialTheme.colorScheme.onSurfaceVariant,
        )
    }
}

/**
 * A library row: cover + title/subtitle + a trailing action, with an optional
 * download progress bar / error line underneath.
 *
 * Flat (SKADI-T-0577). Every row used to be a rounded card three shades
 * lighter than the background with an 8 dp gap to the next; a screenful was
 * a stack of boxes, and the boxes were what the eye landed on. Padding gives
 * the same rhythm without the outlines.
 */
@OptIn(ExperimentalFoundationApi::class)
@Composable
private fun BookRow(
    title: String,
    subtitle: String,
    coverModel: Any?,
    trailing: @Composable () -> Unit,
    onClick: (() -> Unit)? = null,
    progress: Float? = null,
    error: String? = null,
    onLongPress: (() -> Unit)? = null,
) {
    Column(
        modifier = Modifier
            .fillMaxWidth()
            .combinedClickable(onClick = { onClick?.invoke() }, onLongClick = onLongPress)
            .padding(start = 16.dp, end = 8.dp, top = 8.dp, bottom = 8.dp),
    ) {
        Row(
            modifier = Modifier.fillMaxWidth(),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            val shape = RoundedCornerShape(4.dp)
            if (coverModel != null) {
                AsyncImage(
                    model = coverModel,
                    contentDescription = null,
                    contentScale = ContentScale.Crop,
                    modifier = Modifier.size(64.dp).clip(shape),
                )
            } else {
                ArtPlaceholder(Modifier.size(64.dp), shape, poster = false)
            }
            Column(modifier = Modifier.weight(1f).padding(start = 16.dp, end = 8.dp)) {
                Text(
                    title,
                    style = MaterialTheme.typography.bodyLarge,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
                if (subtitle.isNotEmpty()) {
                    Text(
                        subtitle,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                        style = MaterialTheme.typography.bodySmall,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                }
            }
            // A fixed slot so rows line up whether the action is an icon
            // button, a bare icon or a percentage.
            androidx.compose.foundation.layout.Box(
                modifier = Modifier.size(48.dp),
                contentAlignment = Alignment.Center,
            ) { trailing() }
        }
        if (progress != null) {
            LinearProgressIndicator(
                progress = { progress },
                strokeCap = StrokeCap.Round,
                trackColor = MaterialTheme.colorScheme.surfaceVariant,
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(top = 8.dp, end = 8.dp)
                    .height(4.dp),
            )
        }
        if (error != null) {
            Text(
                "$error — tap the download icon to resume",
                color = MaterialTheme.colorScheme.tertiary,
                style = MaterialTheme.typography.bodySmall,
                modifier = Modifier.padding(top = 6.dp),
            )
        }
    }
}


/** One tile of the audiobook wall (SKADI-T-0609). */
private class BookTileData(
    val key: String,
    val title: String,
    val author: String,
    val cover: Any?,
    val onDevice: Boolean,
    val progress: Float?,
    val open: () -> Unit,
    val more: (() -> Unit)?,
)

/**
 * Square-cover grid with the alphabet rail — the movie wall's shape with a 1:1
 * cover. On-device books carry a small badge; a download in flight shows a
 * thin bar under the cover. Long-press opens the book's actions.
 */
@OptIn(ExperimentalFoundationApi::class)
@Composable
private fun BookWall(tiles: List<BookTileData>) {
    val gridState = androidx.compose.foundation.lazy.grid.rememberLazyGridState()
    val scope = rememberCoroutineScope()
    Row(modifier = Modifier.fillMaxSize()) {
        androidx.compose.foundation.lazy.grid.LazyVerticalGrid(
            columns = androidx.compose.foundation.lazy.grid.GridCells.Adaptive(100.dp),
            state = gridState,
            contentPadding = PaddingValues(start = 16.dp, end = 4.dp, top = 4.dp, bottom = 16.dp),
            horizontalArrangement = Arrangement.spacedBy(10.dp),
            verticalArrangement = Arrangement.spacedBy(14.dp),
            modifier = Modifier.weight(1f),
        ) {
            items(tiles, key = { it.key }) { t ->
                Column(
                    modifier = Modifier.combinedClickable(onClick = t.open, onLongClick = t.more),
                ) {
                    val shape = RoundedCornerShape(6.dp)
                    Box {
                        if (t.cover != null) {
                            AsyncImage(
                                model = t.cover,
                                contentDescription = t.title,
                                contentScale = ContentScale.Crop,
                                modifier = Modifier.fillMaxWidth().aspectRatio(1f).clip(shape),
                            )
                        } else {
                            ArtPlaceholder(Modifier.fillMaxWidth().aspectRatio(1f), shape, poster = false)
                        }
                        if (t.onDevice) {
                            Box(
                                modifier = Modifier.align(Alignment.BottomEnd).padding(6.dp)
                                    .clip(RoundedCornerShape(50))
                                    .background(MaterialTheme.colorScheme.secondary)
                                    .padding(4.dp),
                            ) {
                                Icon(
                                    Icons.Filled.Download,
                                    contentDescription = "On this device",
                                    tint = MaterialTheme.colorScheme.onSecondary,
                                    modifier = Modifier.size(14.dp),
                                )
                            }
                        }
                    }
                    t.progress?.let { p ->
                        LinearProgressIndicator(
                            progress = { p },
                            trackColor = MaterialTheme.colorScheme.surfaceVariant,
                            modifier = Modifier.fillMaxWidth().padding(top = 4.dp).height(2.dp),
                        )
                    }
                    Text(
                        t.title,
                        style = MaterialTheme.typography.bodySmall,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                        modifier = Modifier.padding(top = 6.dp),
                    )
                    if (t.author.isNotEmpty()) {
                        Text(
                            t.author,
                            style = MaterialTheme.typography.labelSmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                            maxLines = 1,
                            overflow = TextOverflow.Ellipsis,
                        )
                    }
                }
            }
        }
        AlphabetRail(
            letters = remember(tiles) { tiles.map { sortLetter(it.title) } },
            onJump = { i -> scope.launch { gridState.scrollToItem(i) } },
        )
    }
}


/** The TV list's row shape for audiobook groupings (SKADI-T-0609): a 48×48
 *  cover, bodyLarge title, muted meta line, trailing controls. */
@Composable
private fun CompactRow(
    cover: Any?,
    title: String,
    meta: String,
    onClick: () -> Unit,
    trailing: @Composable androidx.compose.foundation.layout.RowScope.() -> Unit,
) {
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .clickable(onClick = onClick)
            .padding(start = 16.dp, end = 4.dp, top = 8.dp, bottom = 8.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        val shape = RoundedCornerShape(4.dp)
        if (cover != null) {
            AsyncImage(
                model = cover, contentDescription = null, contentScale = ContentScale.Crop,
                modifier = Modifier.size(48.dp).clip(shape),
            )
        } else {
            ArtPlaceholder(Modifier.size(48.dp), shape, poster = false)
        }
        Column(modifier = Modifier.weight(1f).padding(start = 16.dp, end = 8.dp)) {
            Text(title, style = MaterialTheme.typography.bodyLarge, maxLines = 1, overflow = TextOverflow.Ellipsis)
            Text(meta, color = MaterialTheme.colorScheme.onSurfaceVariant, style = MaterialTheme.typography.bodySmall, maxLines = 1, overflow = TextOverflow.Ellipsis)
        }
        trailing()
    }
}
