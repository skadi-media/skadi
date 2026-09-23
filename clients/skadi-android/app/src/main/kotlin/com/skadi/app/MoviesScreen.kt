package com.skadi.app

import androidx.compose.material3.TextButton
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.aspectRatio
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.grid.GridCells
import androidx.compose.foundation.lazy.grid.LazyVerticalGrid
import androidx.compose.foundation.lazy.grid.items
import androidx.compose.foundation.lazy.grid.rememberLazyGridState
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.PlayArrow
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
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
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.draw.clip
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import coil.compose.AsyncImage
import com.skadi.core.Movie
import com.skadi.core.SkadiApi
import com.skadi.core.VideoProgress
import com.skadi.core.WatchRecord
import kotlinx.coroutines.launch

/**
 * The movie library (SKADI-T-0575).
 *
 * A tab, not a sub-screen of the audiobook library (SKADI-T-0577). This screen
 * is streaming-only, deliberately: audiobooks download for offline listening
 * because you listen on a plane; a 1.5 GB film does not belong on a phone by
 * default. So there is no Download here — tapping a poster streams, and the
 * server serves ranges so seeking works (SKADI-T-0574).
 */
@Composable
fun MoviesScreen(
    baseUrl: String,
    token: String,
    onPlay: (url: String, title: String, progressKey: String) -> Unit,
) {
    val api = remember(baseUrl, token) { SkadiApi(baseUrl, token) }
    val context = LocalContext.current
    val progress = remember { VideoProgress(context) }
    // Playing a film records what it is (title, art) so Home can show it
    // without a round trip (SKADI-T-0578); the player then only records where.
    val play = { m: Movie, eid: String ->
        val key = progress.movieKey(m.id, eid)
        progress.start(
            WatchRecord(
                key = key, kind = "movie", parentId = m.id, itemId = eid,
                title = m.title, subtitle = m.year?.toString(), posterUrl = m.poster_url,
            ),
        )
        onPlay(api.movieVideoUrl(m.id, eid), m.displayTitle, key)
    }
    var movies by remember { mutableStateOf(LibraryCache.movies ?: emptyList()) }
    var loading by remember { mutableStateOf(LibraryCache.movies == null) }
    var failed by remember { mutableStateOf(false) }
    var query by rememberSaveable { mutableStateOf("") }
    var genre by rememberSaveable { mutableStateOf<String?>(null) }
    var reloadKey by remember { mutableStateOf(0) }
    var refreshing by remember { mutableStateOf(false) }
    var openCollection by remember { mutableStateOf<MovieTile.Collection?>(null) }
    // A tapped film opens its page; Play lives there (SKADI-T-0586).
    var openMovie by rememberSaveable { mutableStateOf<String?>(null) }

    LaunchedEffect(reloadKey) {
        runCatching { api.listAllMovies() }
            .onSuccess { movies = it; LibraryCache.movies = it; failed = false }
            .onFailure { failed = movies.isEmpty() }
        loading = false
        refreshing = false
    }

    openMovie?.let { id ->
        val m = movies.firstOrNull { it.id == id }
        if (m != null) {
            androidx.activity.compose.BackHandler { openMovie = null }
            MovieDetailScreen(movie = m, api = api, onPlay = { eid -> play(m, eid) }, onBack = { openMovie = null })
            return
        }
    }

    Column(modifier = Modifier.fillMaxSize()) {
        SkadiTopBar(title = "Movies", query = query, onQuery = { query = it }, placeholder = "Search movies")
        GenreChips(genresOf = movies.map { it.genres }, selected = genre, onSelect = { genre = it })

        if (loading) {
            Loading()
            return@Column
        }

        val q = query.trim().lowercase()
        val visible = movies.filter { (q.isEmpty() || it.title.lowercase().contains(q)) && (genre == null || genre in it.genres) }

        if (visible.isEmpty()) {
            // Each case gets its own sentence: "nothing here" and "couldn't ask"
            // are different problems and an operator needs to know which.
            EmptyState(
                when {
                    failed -> "Couldn't reach the server. Check the connection and try again."
                    q.isNotEmpty() -> "No movies match “$query”."
                    else -> "No movies in the library."
                },
                action = { TextButton(onClick = { reloadKey++ }) { Text("Try again") } },
            )
            return@Column
        }

        // A poster GRID, not a list (SKADI-T-0579). One row per film over 1,818
        // films is ~260 screenfuls; three columns of poster art is ~5x the
        // density AND uses the thing people actually recognise a film by.
        //
        // Sorted by `sortKey` so *The Matrix* files under M, matching the rail.
        // Franchises collapse to one tile (SKADI-T-0581). While searching they
        // do NOT: if you typed "taken" you want the films, not a folder that
        // hides them behind another tap.
        val tiles = remember(visible, q) {
            if (q.isEmpty()) {
                collapseCollections(visible)
            } else {
                visible.map { MovieTile.Single(it) }.sortedBy { sortKey(it.sortTitle) }
            }
        }
        val gridState = rememberLazyGridState()
        val scope = rememberCoroutineScope()
        Refreshable(refreshing = refreshing, onRefresh = { refreshing = true; reloadKey++ }) {
Row(modifier = Modifier.fillMaxSize()) {
            LazyVerticalGrid(
                columns = GridCells.Adaptive(100.dp),
                state = gridState,
                contentPadding = PaddingValues(start = 16.dp, end = 4.dp, top = 4.dp, bottom = 16.dp),
                horizontalArrangement = Arrangement.spacedBy(10.dp),
                verticalArrangement = Arrangement.spacedBy(14.dp),
                modifier = Modifier.weight(1f),
            ) {
                items(
                    tiles,
                    key = {
                        when (it) {
                            is MovieTile.Single -> "m:${it.movie.id}"
                            is MovieTile.Collection -> "c:${it.name}"
                        }
                    },
                ) { tile ->
                    when (tile) {
                        is MovieTile.Single -> MoviePoster(
                            movie = tile.movie,
                            onOpen = { openMovie = tile.movie.id },
                        )
                        is MovieTile.Collection -> CollectionPoster(
                            tile = tile,
                            onOpen = { openCollection = tile },
                        )
                    }
                }
            }
            AlphabetRail(
                letters = remember(tiles) { tiles.map { sortLetter(it.sortTitle) } },
                onJump = { i -> scope.launch { gridState.scrollToItem(i) } },
                modifier = Modifier.padding(vertical = 4.dp),
            )
        }
}
    }

    openCollection?.let { tile ->
        CollectionSheet(
            tile = tile,
            onDismiss = { openCollection = null },
            onOpen = { m ->
                openCollection = null
                openMovie = m.id
            },
            onPlay = { m, eid ->
                openCollection = null
                play(m, eid)
            },
        )
    }
}

private val PosterShape = RoundedCornerShape(6.dp)

/** Poster art at 2:3, or a blank slot. Cropping square would cut the title off,
 *  which is the part that identifies it. */
@Composable
private fun Poster(url: String?, description: String?, modifier: Modifier = Modifier) {
    if (url != null) {
        AsyncImage(
            model = url,
            contentDescription = description,
            contentScale = ContentScale.Crop,
            modifier = modifier.aspectRatio(2f / 3f).clip(PosterShape),
        )
    } else {
        ArtPlaceholder(modifier = modifier, shape = PosterShape)
    }
}

/**
 * One film. Tap to play.
 *
 * No play badge (SKADI-T-0577): nearly every film in the library is playable,
 * so a badge on each playable tile was a badge on every tile — decoration, not
 * information. The rare film that is *not* on disk is the exception, and the
 * exception is what gets marked: it is dimmed and does not respond.
 */
@Composable
private fun MoviePoster(movie: Movie, onOpen: () -> Unit) {
    val playable = movie.playableEditionId
    // Every tile opens its page, on disk or not: the page is where "not on
    // disk" gets explained.
    Column(modifier = Modifier.alpha(if (playable != null) 1f else 0.35f).clickable(onClick = onOpen)) {
        Poster(movie.poster_url, movie.title, Modifier.fillMaxWidth())
        Text(
            movie.title,
            style = MaterialTheme.typography.bodySmall,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
            modifier = Modifier.padding(top = 6.dp),
        )
        movie.year?.let {
            Text(
                it.toString(),
                style = MaterialTheme.typography.labelSmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
    }
}

/** A collapsed franchise: the earliest film's poster with a count. The count
 *  is the whole signal that this tile is a folder and not a film. */
@Composable
private fun CollectionPoster(tile: MovieTile.Collection, onOpen: () -> Unit) {
    Column(modifier = Modifier.clickable(onClick = onOpen)) {
        Box {
            Poster(tile.films.firstNotNullOfOrNull { it.poster_url }, tile.name, Modifier.fillMaxWidth())
            Box(
                modifier = Modifier.align(Alignment.TopEnd).padding(6.dp)
                    .clip(RoundedCornerShape(50))
                    .background(MaterialTheme.colorScheme.secondary)
                    .padding(horizontal = 7.dp, vertical = 2.dp),
            ) {
                Text(
                    "${tile.films.size}",
                    style = MaterialTheme.typography.labelMedium,
                    color = MaterialTheme.colorScheme.onSecondary,
                )
            }
        }
        Text(
            tile.name,
            style = MaterialTheme.typography.bodySmall,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
            modifier = Modifier.padding(top = 6.dp),
        )
        Text(
            "${tile.films.size} films",
            style = MaterialTheme.typography.labelSmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
    }
}

/** The films inside a franchise, in release order. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun CollectionSheet(
    tile: MovieTile.Collection,
    onDismiss: () -> Unit,
    onOpen: (Movie) -> Unit,
    onPlay: (Movie, String) -> Unit,
) {
    ModalBottomSheet(
        onDismissRequest = onDismiss,
        containerColor = MaterialTheme.colorScheme.surfaceContainer,
    ) {
        Text(
            tile.name,
            style = MaterialTheme.typography.titleLarge,
            modifier = Modifier.padding(horizontal = 24.dp, vertical = 8.dp),
        )
        // A plain scrolling Column: a franchise is a handful of films.
        Column(
            modifier = Modifier
                .padding(bottom = 16.dp)
                .verticalScroll(rememberScrollState()),
        ) {
            tile.films.forEach { m ->
                val eid = m.playableEditionId
                Row(
                    modifier = Modifier
                        .fillMaxWidth()
                        .clickable { onOpen(m) }
                        .padding(horizontal = 24.dp, vertical = 8.dp)
                        // A film in the collection that is not on disk still gets
                        // a row, dimmed: knowing the gap is part of why you
                        // opened the franchise at all.
                        .alpha(if (eid != null) 1f else 0.4f),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    Poster(m.poster_url, null, Modifier.size(width = 44.dp, height = 66.dp))
                    Column(modifier = Modifier.weight(1f).padding(start = 16.dp)) {
                        Text(m.title, style = MaterialTheme.typography.bodyLarge, maxLines = 2)
                        Text(
                            m.year?.toString() ?: (if (eid == null) "not on disk" else ""),
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                    }
                    if (eid != null) {
                        IconButton(onClick = { onPlay(m, eid) }) {
                            Icon(
                                Icons.Filled.PlayArrow,
                                contentDescription = "Play ${m.title}",
                                tint = MaterialTheme.colorScheme.primary,
                            )
                        }
                    }
                }
            }
        }
    }
}
