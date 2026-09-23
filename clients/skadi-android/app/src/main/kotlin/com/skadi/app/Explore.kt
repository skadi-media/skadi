package com.skadi.app

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.AssistChip
import androidx.compose.material3.FilterChip
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
import androidx.compose.ui.draw.clip
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import coil.compose.AsyncImage
import com.skadi.core.Book
import com.skadi.core.SkadiApi
import com.skadi.core.Work
import kotlinx.coroutines.launch

/**
 * Audiobook explore (SKADI-T-0604): an author's or a series' whole body of
 * work, not just the slice in the library. Rows come from
 * `/audiobooks/works` (owned + unowned, series order) when the name resolves
 * to an Audible ASIN; the library's own books fill in when it does not.
 */

/** Case/diacritic-insensitive key for matching names across sources. */
internal fun nameKey(s: String): String = s.lowercase().filter { it.isLetterOrDigit() && it.code < 128 }

/** One row of a body of work: a library book, a known-but-unowned work, or both. */
private data class WorkRow(
    val key: String,
    val title: String,
    val cover: String?,
    val series: String?,
    val position: String?,
    val year: String?,
    val book: Book?,
    val asin: String?,
) {
    val owned: Boolean get() = book?.importedFileId != null
    val wanted: Boolean get() = book != null && book.importedFileId == null
}

private fun posKey(p: String?): Double = p?.trim()?.toDoubleOrNull() ?: Double.MAX_VALUE

/** Library books plus works, de-duplicated by ASIN then by title. */
private fun mergeRows(library: List<Book>, works: List<Work>): List<WorkRow> {
    val byAsin = library.associateBy { (it.external_ids?.asin ?: it.asin)?.uppercase() }
    val byTitle = library.associateBy { nameKey(it.title) }
    val rows = mutableListOf<WorkRow>()
    val seen = mutableSetOf<String>()
    for (w in works) {
        val b = byAsin[w.asin.uppercase()] ?: byTitle[nameKey(w.title)]
        b?.let { seen += it.id }
        rows += WorkRow(
            key = "w:" + w.asin, title = w.title, cover = w.coverUrl ?: b?.coverUrl,
            series = w.seriesName ?: b?.series?.name, position = w.seriesPosition ?: b?.series?.position,
            year = w.releaseDate?.take(4) ?: b?.year?.toString(), book = b, asin = w.asin,
        )
    }
    for (b in library) {
        if (b.id in seen) continue
        rows += WorkRow(
            key = "b:" + b.id, title = b.title, cover = b.coverUrl, series = b.series?.name,
            position = b.series?.position, year = b.year?.toString(), book = b, asin = b.external_ids?.asin ?: b.asin,
        )
    }
    return rows
}

@Composable
fun AuthorScreen(
    name: String,
    api: SkadiApi,
    library: List<Book>,
    onOpenBook: (String) -> Unit,
    onBack: () -> Unit,
) {
    val scope = rememberCoroutineScope()
    val mine = remember(library, name) { library.filter { b -> b.authors.any { nameKey(it) == nameKey(name) } } }
    var asin by remember(name) { mutableStateOf<String?>(null) }
    var image by remember(name) { mutableStateOf<String?>(null) }
    var works by remember(name) { mutableStateOf<List<Work>?>(null) }
    var watching by remember(name) { mutableStateOf<Boolean?>(null) }
    var resolved by remember(name) { mutableStateOf(false) }
    var added by remember { mutableStateOf(setOf<String>()) }
    var filter by remember { mutableStateOf("all") }

    LaunchedEffect(name) {
        // Registered (watched) authors carry their ASIN; anyone else goes
        // through one Audible lookup, best match first.
        val a = runCatching { api.listAuthors() }.getOrDefault(emptyList())
            .firstOrNull { it.asin != null && nameKey(it.name) == nameKey(name) }?.asin
            ?: runCatching { api.lookupAuthors(name) }.getOrDefault(emptyList())
                .firstOrNull { nameKey(it.name) == nameKey(name) }?.also { image = it.image }?.asin
        asin = a
        if (a != null) {
            works = runCatching { api.listWorks(author = a) }.getOrNull()
            watching = runCatching { api.listWatchers() }.getOrNull()?.any { it.scope == "author" && it.key == a }
        }
        resolved = true
    }

    val rows = remember(mine, works) { mergeRows(mine, works ?: emptyList()) }
    val owned = rows.count { it.owned }
    val wanted = rows.count { it.wanted }
    val missing = rows.size - owned - wanted

    Column(modifier = Modifier.fillMaxSize()) {
        SkadiTopBar(title = name, onBack = onBack)
        Row(
            verticalAlignment = Alignment.CenterVertically,
            modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 4.dp),
        ) {
            image?.let {
                AsyncImage(
                    model = it, contentDescription = null, contentScale = ContentScale.Crop,
                    modifier = Modifier.size(56.dp).clip(RoundedCornerShape(28.dp)),
                )
                Spacer(Modifier.width(12.dp))
            }
            Text(
                listOfNotNull(
                    "$owned owned",
                    wanted.takeIf { it > 0 }?.let { "$it wanted" },
                    missing.takeIf { it > 0 }?.let { "$it not in library" },
                ).joinToString(" · "),
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.weight(1f),
            )
            val a = asin
            if (a != null && watching != null) {
                TextButton(onClick = {
                    val now = watching == true
                    watching = !now
                    scope.launch {
                        val ok = if (now) api.clearWatcher("author", a) else api.setWatcher("author", a)
                        if (!ok) watching = now
                    }
                }) { Text(if (watching == true) "Watching" else "Watch author") }
            }
        }
        if (resolved && asin == null) {
            Text(
                "Not found on Audible — showing what's in the library.",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.padding(horizontal = 16.dp),
            )
        }
        Row(
            horizontalArrangement = Arrangement.spacedBy(8.dp),
            modifier = Modifier.padding(horizontal = 16.dp, vertical = 4.dp),
        ) {
            FilterChip(selected = filter == "all", onClick = { filter = "all" }, label = { Text("All (${rows.size})") })
            FilterChip(selected = filter == "owned", onClick = { filter = "owned" }, label = { Text("Owned ($owned)") })
            FilterChip(selected = filter == "missing", onClick = { filter = "missing" }, label = { Text("Not owned (${rows.size - owned})") })
        }
        if (!resolved && mine.isEmpty()) { Loading(); return@Column }
        val shown = when (filter) {
            "owned" -> rows.filter { it.owned }
            "missing" -> rows.filter { !it.owned }
            else -> rows
        }
        if (shown.isEmpty()) { EmptyState("Nothing here."); return@Column }
        // Grouped by series, in reading order; standalone titles last.
        val groups = shown.groupBy { it.series ?: "" }.entries
            .sortedWith(compareBy({ it.key.isEmpty() }, { it.key.lowercase() }))
        LazyColumn(modifier = Modifier.fillMaxSize(), contentPadding = PaddingValues(bottom = 24.dp)) {
            groups.forEach { (series, list) ->
                item(key = "h:$series") {
                    Text(
                        series.ifEmpty { "Standalone" },
                        style = MaterialTheme.typography.labelLarge,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                        modifier = Modifier.padding(start = 16.dp, top = 12.dp, bottom = 4.dp),
                    )
                }
                items(list.sortedWith(compareBy({ posKey(it.position) }, { it.title.lowercase() })), key = { it.key }) { r ->
                    WorkRowView(
                        r = r,
                        added = r.asin in added,
                        onOpen = { r.book?.let { onOpenBook(it.id) } },
                        onAdd = {
                            val a = r.asin ?: return@WorkRowView
                            scope.launch { if (api.addBook(a)) added = added + a }
                        },
                    )
                }
            }
        }
    }
}

@Composable
fun SeriesExploreScreen(
    name: String,
    seriesAsin: String?,
    api: SkadiApi,
    library: List<Book>,
    onOpenBook: (String) -> Unit,
    onBack: () -> Unit,
) {
    val scope = rememberCoroutineScope()
    val mine = remember(library, name) { library.filter { nameKey(it.series?.name ?: "") == nameKey(name) } }
    var works by remember(name) { mutableStateOf<List<Work>?>(null) }
    var watching by remember(name) { mutableStateOf<Boolean?>(null) }
    var resolved by remember(name) { mutableStateOf(false) }
    var added by remember { mutableStateOf(setOf<String>()) }

    LaunchedEffect(name, seriesAsin) {
        if (seriesAsin != null) {
            works = runCatching { api.listWorks(series = seriesAsin) }.getOrNull()
            watching = runCatching { api.listWatchers() }.getOrNull()?.any { it.scope == "series" && it.key == seriesAsin }
        }
        resolved = true
    }
    val rows = remember(mine, works) {
        mergeRows(mine, works ?: emptyList()).sortedWith(compareBy({ posKey(it.position) }, { it.title.lowercase() }))
    }
    val owned = rows.count { it.owned }

    Column(modifier = Modifier.fillMaxSize()) {
        SkadiTopBar(title = name, onBack = onBack)
        Row(
            verticalAlignment = Alignment.CenterVertically,
            modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 4.dp),
        ) {
            Text(
                if (rows.isEmpty()) "" else "$owned of ${rows.size} owned" +
                    (rows.count { it.wanted }.takeIf { it > 0 }?.let { " · $it wanted" } ?: "") +
                    ((rows.size - owned - rows.count { it.wanted }).takeIf { it > 0 }?.let { " · $it gaps" } ?: ""),
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.weight(1f),
            )
            if (seriesAsin != null && watching != null) {
                TextButton(onClick = {
                    val now = watching == true
                    watching = !now
                    scope.launch {
                        val ok = if (now) api.clearWatcher("series", seriesAsin) else api.setWatcher("series", seriesAsin)
                        if (!ok) watching = now
                    }
                }) { Text(if (watching == true) "Watching" else "Watch series") }
            }
        }
        if (resolved && seriesAsin == null) {
            Text(
                "Series not matched on Audible — showing what's in the library.",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.padding(horizontal = 16.dp),
            )
        }
        if (!resolved && mine.isEmpty()) { Loading(); return@Column }
        if (rows.isEmpty()) { EmptyState("Nothing here."); return@Column }
        LazyColumn(modifier = Modifier.fillMaxSize(), contentPadding = PaddingValues(bottom = 24.dp)) {
            items(rows, key = { it.key }) { r ->
                WorkRowView(
                    r = r,
                    added = r.asin in added,
                    onOpen = { r.book?.let { onOpenBook(it.id) } },
                    onAdd = {
                        val a = r.asin ?: return@WorkRowView
                        scope.launch { if (api.addBook(a)) added = added + a }
                    },
                )
            }
        }
    }
}

@Composable
private fun WorkRowView(r: WorkRow, added: Boolean, onOpen: () -> Unit, onAdd: () -> Unit) {
    Row(
        verticalAlignment = Alignment.CenterVertically,
        modifier = Modifier
            .fillMaxWidth()
            .clickable(enabled = r.book != null, onClick = onOpen)
            .padding(start = 16.dp, end = 8.dp, top = 6.dp, bottom = 6.dp),
    ) {
        val shape = RoundedCornerShape(4.dp)
        if (r.cover != null) {
            AsyncImage(
                model = r.cover, contentDescription = null, contentScale = ContentScale.Crop,
                modifier = Modifier.size(48.dp).clip(shape),
            )
        } else {
            ArtPlaceholder(modifier = Modifier.size(48.dp), shape = shape, poster = false)
        }
        Spacer(Modifier.width(12.dp))
        Column(modifier = Modifier.weight(1f)) {
            Text(
                listOfNotNull(r.position?.let { "#$it" }, r.title).joinToString(" "),
                style = MaterialTheme.typography.bodyLarge, maxLines = 2, overflow = TextOverflow.Ellipsis,
            )
            r.year?.let { Text(it, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant) }
        }
        Spacer(Modifier.width(8.dp))
        when {
            r.owned -> Text("Owned", style = MaterialTheme.typography.labelLarge, color = MaterialTheme.colorScheme.primary)
            r.wanted -> Text("Wanted", style = MaterialTheme.typography.labelLarge, color = MaterialTheme.colorScheme.tertiary)
            added -> Text("Added", style = MaterialTheme.typography.labelLarge, color = MaterialTheme.colorScheme.primary)
            r.asin != null -> AssistChip(onClick = onAdd, label = { Text("Add") })
        }
    }
}
