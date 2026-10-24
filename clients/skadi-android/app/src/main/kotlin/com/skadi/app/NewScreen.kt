package com.skadi.app

import androidx.compose.foundation.clickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.FilterChip
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.derivedStateOf
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.runtime.snapshotFlow
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import coil.compose.AsyncImage
import com.skadi.core.NEW_IMPORT_KINDS
import com.skadi.core.NewImport
import com.skadi.core.SkadiApi
import com.skadi.core.kindTag
import com.skadi.core.mergeNewer
import com.skadi.core.subtitle
import kotlinx.coroutines.launch

/** Rows per page. */
private const val PAGE_ROWS = 30

/** Refresh cadence while the tab is shown. Imports land minutes apart at most. */
private const val NEW_POLL_MS = 60_000L

/**
 * The New tab (SKADI-T-0746, initiative COLLIERY-I-0653): media new to the
 * library, newest first, as the server's `GET /new-imports` gives it for this
 * member — the server applies the household policy, so a kid's list and a
 * parent's differ with no logic here.
 *
 * Refreshes when the tab opens and every minute while it is shown (new rows go
 * on top; what is scrolled in stays), pulls down to refresh, and loads the next
 * page as the end of the list comes into view.
 */
@Composable
fun NewScreen(
    baseUrl: String,
    token: String,
    /** Open the item a row names: (kind, item id, season for an episode). */
    onOpen: (kind: String, itemId: String, season: Int?) -> Unit,
    /** This week's count changed (the tab badge). */
    onCount: (Int) -> Unit = {},
) {
    val api = remember(baseUrl, token) { SkadiApi(baseUrl, token) }
    val scope = rememberCoroutineScope()
    var kind by rememberSaveable { mutableStateOf<String?>(null) }
    var rows by remember { mutableStateOf<List<NewImport>?>(null) }
    var cursor by remember { mutableStateOf<String?>(null) }
    var done by remember { mutableStateOf(false) }
    var busy by remember { mutableStateOf(false) }
    var failed by remember { mutableStateOf(false) }
    var refreshing by remember { mutableStateOf(false) }
    var reloadKey by remember { mutableStateOf(0) }

    // First page, again on a filter change or a pull-to-refresh.
    LaunchedEffect(kind, reloadKey) {
        busy = true
        val k = kind
        runCatching { api.newImports(k, null, PAGE_ROWS) }
            .onSuccess { page ->
                if (k == kind) {
                    rows = page.items
                    cursor = page.nextCursor
                    done = page.nextCursor == null
                    failed = false
                }
            }
            .onFailure { failed = rows == null }
        runCatching { api.newImportsCount() }.onSuccess(onCount)
        busy = false
        refreshing = false
    }

    fun loadMore() {
        val after = cursor ?: return
        if (busy || done) return
        busy = true
        val k = kind
        scope.launch {
            runCatching { api.newImports(k, after, PAGE_ROWS) }
                .onSuccess { page ->
                    if (k == kind) {
                        rows = (rows ?: emptyList()) + page.items
                        cursor = page.nextCursor
                        done = page.nextCursor == null
                    }
                }
            busy = false
        }
    }

    // Live while shown: new rows on top.
    PollEffect(key = kind, baseMs = NEW_POLL_MS) {
        val k = kind
        if (rows == null) return@PollEffect
        runCatching { api.newImports(k, null, PAGE_ROWS) }
            .onSuccess { page -> if (k == kind) rows = mergeNewer(rows ?: emptyList(), page.items) }
        runCatching { api.newImportsCount() }.onSuccess(onCount)
    }

    val listState = rememberLazyListState()
    val nearEnd by remember {
        derivedStateOf {
            val last = listState.layoutInfo.visibleItemsInfo.lastOrNull()?.index ?: 0
            last >= listState.layoutInfo.totalItemsCount - 5
        }
    }
    LaunchedEffect(listState) {
        snapshotFlow { nearEnd }.collect { if (it) loadMore() }
    }

    Scaffold(
        containerColor = MaterialTheme.colorScheme.background,
        topBar = { SkadiTopBar(title = "New") },
    ) { inset ->
        val items = rows
        when {
            items == null && failed -> EmptyState("Couldn't reach the server.") {
                TextButton(onClick = { reloadKey++ }) { Text("Try again") }
            }
            items == null -> Loading()
            else -> Refreshable(refreshing = refreshing, onRefresh = { refreshing = true; reloadKey++ }) {
                LazyColumn(
                    state = listState,
                    modifier = Modifier.fillMaxSize().padding(inset),
                    contentPadding = PaddingValues(bottom = 24.dp),
                ) {
                    item {
                        Row(
                            horizontalArrangement = Arrangement.spacedBy(8.dp),
                            modifier = Modifier
                                .horizontalScroll(rememberScrollState())
                                .padding(horizontal = 16.dp, vertical = 4.dp),
                        ) {
                            NEW_IMPORT_KINDS.forEach { (value, label) ->
                                FilterChip(selected = kind == value, onClick = { kind = value }, label = { Text(label) })
                            }
                        }
                    }
                    if (items.isEmpty()) {
                        item { EmptyState("Nothing new yet.") }
                    }
                    items(items, key = { it.id }) { n ->
                        NewImportRow(n, onClick = { onOpen(n.kind, n.itemId, com.skadi.core.seasonOf(n.episode)) })
                        HorizontalDivider()
                    }
                    if (!done && items.isNotEmpty()) {
                        item {
                            Row(Modifier.fillMaxWidth().padding(16.dp), horizontalArrangement = Arrangement.Center) {
                                CircularProgressIndicator(Modifier.width(24.dp).height(24.dp))
                            }
                        }
                    }
                }
            }
        }
    }
}

/** One new import: poster, title, subtitle, media tag, Upgrade, quality, age. */
@Composable
private fun NewImportRow(n: NewImport, onClick: () -> Unit) {
    val shape = RoundedCornerShape(4.dp)
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .clickable(onClick = onClick)
            .padding(horizontal = 16.dp, vertical = 8.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        if (n.posterUrl != null) {
            AsyncImage(
                model = n.posterUrl,
                contentDescription = null,
                contentScale = ContentScale.Crop,
                modifier = Modifier.width(40.dp).height(60.dp).clip(shape),
            )
        } else {
            ArtPlaceholder(Modifier.width(40.dp).height(60.dp), shape)
        }
        Spacer(Modifier.width(12.dp))
        Column(Modifier.weight(1f)) {
            Text(n.title, style = MaterialTheme.typography.bodyLarge, maxLines = 1, overflow = TextOverflow.Ellipsis)
            val sub = n.subtitle()
            if (sub.isNotEmpty()) {
                Text(
                    sub,
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
            }
            Row(horizontalArrangement = Arrangement.spacedBy(6.dp), verticalAlignment = Alignment.CenterVertically) {
                Tag(n.kindTag(), MaterialTheme.colorScheme.secondaryContainer, MaterialTheme.colorScheme.onSecondaryContainer)
                if (n.upgrade) {
                    Tag("Upgrade", MaterialTheme.colorScheme.tertiaryContainer, MaterialTheme.colorScheme.onTertiaryContainer)
                }
                n.quality?.let {
                    Text(it, style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                }
            }
        }
        ago(n.at)?.let {
            Text(it, style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
        }
    }
}

@Composable
private fun Tag(text: String, container: Color, content: Color) {
    Surface(color = container, contentColor = content, shape = RoundedCornerShape(50)) {
        Text(text, style = MaterialTheme.typography.labelSmall, modifier = Modifier.padding(horizontal = 6.dp, vertical = 1.dp))
    }
}
