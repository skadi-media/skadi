package com.skadi.app

import androidx.compose.material3.TextButton
import androidx.activity.compose.BackHandler
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.verticalScroll
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.PlayArrow
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FilterChip
import androidx.compose.material3.FilterChipDefaults
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
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
import androidx.compose.ui.draw.clip
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import coil.compose.AsyncImage
import com.skadi.core.Series
import com.skadi.core.SkadiApi
import com.skadi.core.VideoProgress
import com.skadi.core.WatchRecord
import kotlinx.coroutines.launch

/**
 * The TV library: series list, then episodes by season (SKADI-T-0578).
 *
 * Two screens rather than one, because TV has a level movies do not — you pick a
 * show, then an episode. Flattening 24,000 episodes into one list would be
 * unusable, and is also why the list view asks for the *summary* projection: the
 * full shape is 17.5 MB over this library (SKADI-T-0494).
 */
@Composable
fun TvScreen(
    baseUrl: String,
    token: String,
    onPlay: (url: String, title: String, progressKey: String) -> Unit,
    /** A show to open at a season, asked for by another tab (SKADI-T-0608);
     *  consumed once via [onRequestConsumed]. */
    request: Pair<String, Int>? = null,
    onRequestConsumed: () -> Unit = {},
) {
    val api = remember(baseUrl, token) { SkadiApi(baseUrl, token) }
    var series by remember { mutableStateOf(LibraryCache.series ?: emptyList()) }
    var loading by remember { mutableStateOf(LibraryCache.series == null) }
    var failed by remember { mutableStateOf(false) }
    var query by rememberSaveable { mutableStateOf("") }
    var genre by rememberSaveable { mutableStateOf<String?>(null) }
    var reloadKey by remember { mutableStateOf(0) }
    var refreshing by remember { mutableStateOf(false) }
    var openSeriesId by rememberSaveable { mutableStateOf<String?>(null) }
    var requestedSeason by rememberSaveable { mutableStateOf<Int?>(null) }
    LaunchedEffect(request) {
        request?.let { (sid, season) ->
            requestedSeason = season
            openSeriesId = sid
            onRequestConsumed()
        }
    }

    LaunchedEffect(reloadKey) {
        runCatching { api.listAllSeries() }
            .onSuccess { series = it; LibraryCache.series = it; failed = false }
            .onFailure { failed = series.isEmpty() }
        loading = false
        refreshing = false
    }

    openSeriesId?.let { sid ->
        BackHandler { openSeriesId = null }
        SeriesDetailScreen(
            api = api,
            seriesId = sid,
            initialSeason = requestedSeason,
            onPlay = onPlay,
            onBack = { openSeriesId = null; requestedSeason = null },
        )
        return
    }

    Column(modifier = Modifier.fillMaxSize()) {
        SkadiTopBar(title = "TV", query = query, onQuery = { query = it }, placeholder = "Search shows")
        GenreChips(genresOf = series.map { it.genres }, selected = genre, onSelect = { genre = it })

        if (loading) {
            Loading()
            return@Column
        }

        val q = query.trim().lowercase()
        val visible = series.filter { (q.isEmpty() || it.title.lowercase().contains(q)) && (genre == null || genre in it.genres) }
        if (visible.isEmpty()) {
            EmptyState(
                when {
                    failed -> "Couldn't reach the server. Check the connection and try again."
                    q.isNotEmpty() -> "No shows match “$query”."
                    else -> "No shows in the library."
                },
                action = { TextButton(onClick = { reloadKey++ }) { Text("Try again") } },
            )
            return@Column
        }

        // Sorted and railed like the movie grid (SKADI-T-0579): the server's
        // own order is not article-aware.
        val sorted = remember(visible) { visible.sortedBy { sortKey(it.title) } }
        val listState = rememberLazyListState()
        val scope = rememberCoroutineScope()
        Refreshable(refreshing = refreshing, onRefresh = { refreshing = true; reloadKey++ }) {
Row(modifier = Modifier.fillMaxSize()) {
            LazyColumn(
                state = listState,
                contentPadding = PaddingValues(bottom = 16.dp),
                modifier = Modifier.weight(1f),
            ) {
                items(sorted, key = { it.id }) { s ->
                    SeriesRow(series = s, onOpen = { openSeriesId = s.id })
                }
            }
            AlphabetRail(
                letters = remember(sorted) { sorted.map { sortLetter(it.title) } },
                onJump = { i -> scope.launch { listState.scrollToItem(i) } },
                modifier = Modifier.padding(vertical = 4.dp),
            )
        }
}
    }
}

/** A flat list row (SKADI-T-0577): art, title, one line of facts. The whole
 *  row opens the show; no chevron, no card. */
@Composable
private fun SeriesRow(series: Series, onOpen: () -> Unit) {
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .clickable(onClick = onOpen)
            .padding(start = 16.dp, end = 8.dp, top = 8.dp, bottom = 8.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        val shape = RoundedCornerShape(4.dp)
        if (series.poster_url != null) {
            AsyncImage(
                model = series.poster_url,
                contentDescription = null,
                contentScale = ContentScale.Crop,
                modifier = Modifier.size(width = 48.dp, height = 72.dp).clip(shape),
            )
        } else {
            ArtPlaceholder(Modifier.size(width = 48.dp, height = 72.dp), shape, poster = false)
        }
        Column(modifier = Modifier.weight(1f).padding(start = 16.dp)) {
            Text(
                series.title,
                style = MaterialTheme.typography.bodyLarge,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
            val n = series.playableCount
            Text(
                listOfNotNull(
                    series.year?.toString(),
                    series.network,
                    // The count that matters is what you can watch, not what the
                    // show has — a series with 62 episodes and 3 on disk is a
                    // different proposition.
                    if (n > 0) "$n episode${if (n == 1) "" else "s"}" else "nothing on disk",
                ).joinToString(" · "),
                color = if (n > 0) {
                    MaterialTheme.colorScheme.onSurfaceVariant
                } else {
                    MaterialTheme.colorScheme.tertiary
                },
                style = MaterialTheme.typography.bodySmall,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
        }
    }
}

/** One series: a season picker over its playable episodes. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun SeriesDetailScreen(
    api: SkadiApi,
    seriesId: String,
    initialSeason: Int? = null,
    onPlay: (url: String, title: String, progressKey: String) -> Unit,
    onBack: () -> Unit,
) {
    val context = LocalContext.current
    val progress = remember { VideoProgress(context) }
    var series by remember(seriesId) { mutableStateOf<Series?>(null) }
    var loading by remember(seriesId) { mutableStateOf(true) }
    var season by rememberSaveable(seriesId) { mutableStateOf<Int?>(initialSeason) }
    var openEpisode by rememberSaveable(seriesId) { mutableStateOf<String?>(null) }
    LaunchedEffect(initialSeason) { initialSeason?.let { season = it } }

    LaunchedEffect(seriesId) {
        // The FULL shape here, not the summary: episode titles and air dates are
        // what make a list of episodes readable, and the summary drops them.
        runCatching { api.series(seriesId) }.onSuccess { series = it }
        loading = false
    }

    // An episode's page, over the list (SKADI-T-0586).
    val openEp = openEpisode?.let { id -> series?.episodes?.firstOrNull { it.id == id } }
    if (openEp != null && series != null) {
        val s = series!!
        BackHandler { openEpisode = null }
        EpisodeDetailScreen(
            series = s,
            episode = openEp,
            api = api,
            onPlay = { playEpisode(progress, api, s, openEp, onPlay) },
            onBack = { openEpisode = null },
            // The series page is right underneath: pop to it at this season.
            onOpenSeries = { season = openEp.season; openEpisode = null },
        )
        return
    }

    Column(modifier = Modifier.fillMaxSize().verticalScroll(rememberScrollState())) {
        val s = series
        if (loading || s == null) {
            SkadiTopBar(title = "", onBack = onBack)
            if (loading) Loading() else EmptyState("Couldn't load this show.")
            return@Column
        }
        SeriesHeader(series = s, onBack = onBack)

        val withContent = s.seasonsWithContent
        if (withContent.isEmpty()) {
            EmptyState("Nothing on disk for this show yet.")
            return@Column
        }
        val chosen = season ?: withContent.first()

        // One scrolling row, not a wrapping one: seven seasons wrapped to three
        // rows of chips and pushed the first episode below the fold.
        Row(
            horizontalArrangement = Arrangement.spacedBy(8.dp),
            modifier = Modifier
                .fillMaxWidth()
                .horizontalScroll(rememberScrollState())
                .padding(horizontal = 16.dp, vertical = 4.dp),
        ) {
            withContent.forEach { n ->
                FilterChip(
                    selected = n == chosen,
                    onClick = { season = n },
                    // Season 0 is Specials everywhere in this codebase; showing
                    // "Season 0" would be a leak of the data model.
                    label = { Text(if (n == 0) "Specials" else "Season $n") },
                    colors = FilterChipDefaults.filterChipColors(
                        selectedContainerColor = MaterialTheme.colorScheme.secondaryContainer,
                        selectedLabelColor = MaterialTheme.colorScheme.onSecondaryContainer,
                    ),
                )
            }
        }

        val eps = s.episodes.filter { it.season == chosen && it.playable }
            .sortedBy { it.number }
        // A plain Column inside the page's scroll: the whole page scrolls as
        // one, header included, rather than a list fighting a scroll view.
        Column(modifier = Modifier.padding(bottom = 16.dp)) {
            eps.forEach { e ->
                val play = { playEpisode(progress, api, s, e, onPlay) }
                // The row opens the episode's page; the icon at the end plays
                // it straight away (SKADI-T-0586).
                Row(
                    modifier = Modifier
                        .fillMaxWidth()
                        .clickable { openEpisode = e.id }
                        .padding(horizontal = 16.dp, vertical = 12.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    // The season is already chosen above; only the episode
                    // number carries information here.
                    Text(
                        "E%02d".format(e.number),
                        style = MaterialTheme.typography.labelLarge,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                        modifier = Modifier.padding(end = 16.dp),
                    )
                    Column(modifier = Modifier.weight(1f)) {
                        Text(
                            e.title ?: "Episode ${e.number}",
                            style = MaterialTheme.typography.bodyLarge,
                            maxLines = 2,
                            overflow = TextOverflow.Ellipsis,
                        )
                        e.air_date?.let {
                            Text(
                                it,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                                style = MaterialTheme.typography.bodySmall,
                            )
                        }
                    }
                    IconButton(onClick = play) {
                        Icon(
                            Icons.Filled.PlayArrow,
                            contentDescription = "Play ${e.code}",
                            tint = MaterialTheme.colorScheme.primary,
                        )
                    }
                }
            }
        }
    }
}

/** Record what is about to play (for Home) and hand it to the player. */
/** Record the start and hand the episode to the player; shared with Home (SKADI-T-0603). */
internal fun playEpisode(
    progress: VideoProgress,
    api: SkadiApi,
    s: Series,
    e: com.skadi.core.Episode,
    onPlay: (url: String, title: String, progressKey: String) -> Unit,
) {
    val key = progress.episodeKey(s.id, e.id)
    progress.start(
        WatchRecord(
            key = key, kind = "episode", parentId = s.id, itemId = e.id,
            title = s.title,
            subtitle = listOfNotNull(e.code, e.title).joinToString(" · "),
            posterUrl = s.poster_url, season = e.season, number = e.number,
        ),
    )
    onPlay(api.episodeVideoUrl(s.id, e.id), "${s.title} · ${e.code}", key)
}
