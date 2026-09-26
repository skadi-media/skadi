package com.skadi.app

import androidx.activity.compose.BackHandler
import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.aspectRatio
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.LazyRow
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.AccountCircle
import androidx.compose.material.icons.filled.CloudOff
import androidx.compose.material.icons.filled.Download
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.LinearProgressIndicator
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
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import coil.compose.AsyncImage
import com.skadi.core.Episode
import com.skadi.core.NotAllowedException
import com.skadi.core.OfflineBook
import com.skadi.core.OfflineStore
import com.skadi.core.Series
import com.skadi.core.SkadiApi
import com.skadi.core.VideoProgress
import com.skadi.core.WatchRecord
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

/**
 * Home: pick up where you left off (SKADI-T-0578).
 *
 * Three rows, each shown only when it has something in it:
 *
 * - **Continue watching** — films and episodes with a saved position.
 * - **Series you're watching** (SKADI-T-0603) — one card per show you are
 *   mid-way through, pointing at the next unwatched episode: the one you
 *   left half-watched, else the one after the last you finished. Tapping
 *   opens that episode's page, with Play. Gone once the show is watched out.
 * - **Up next** — the next book in a series after one you finished. Finishing
 *   something is the moment a library app usually goes quiet; this is where
 *   it should speak up instead.
 * - **Keep listening** — downloaded books with a position, most recent first.
 *
 * Everything on this screen comes from the device: [VideoProgress] for video,
 * [OfflineStore] for books. The one network call is fetching a show's episode
 * list to work out what "next" is, and the row renders without it.
 */
@Composable
fun HomeScreen(
    baseUrl: String,
    token: String,
    onPlayVideo: (url: String, title: String, progressKey: String) -> Unit,
    onPlayBook: (fid: String) -> Unit,
    /** Jump to the TV tab's show page at a season (SKADI-T-0608). */
    onOpenSeries: (seriesId: String, season: Int) -> Unit = { _, _ -> },
    /** Operators get the status line; everyone else has nothing to act on. */
    isAdmin: Boolean = false,
    onOpenDownloads: () -> Unit = {},
    onManageStorage: () -> Unit = {},
    onChangePassword: () -> Unit = {},
    onSignOut: () -> Unit = {},
) {
    val context = LocalContext.current
    val api = remember(baseUrl, token) { SkadiApi(baseUrl, token) }
    val progress = remember { VideoProgress(context) }
    val store = remember { OfflineStore(context.filesDir) }

    var watching by remember { mutableStateOf<List<WatchRecord>>(emptyList()) }
    var upNext by remember { mutableStateOf<List<UpNext>>(emptyList()) }
    var shows by remember { mutableStateOf<List<ShowInProgress>>(emptyList()) }
    var openShow by remember { mutableStateOf<ShowInProgress?>(null) }
    var listening by remember { mutableStateOf<List<Listening>>(emptyList()) }
    var loaded by remember { mutableStateOf(false) }
    var reload by remember { mutableStateOf(0) }
    // One dialog for every shelf. Modelled on what the dialog needs — a name, a
    // line saying what survives, and something to run — rather than on a record
    // type, because the shelves hold three different ones and a per-type
    // `confirmForgetX` would have multiplied a near-identical dialog
    // (SKADI-T-0647).
    var confirmForget by remember { mutableStateOf<ForgetTarget?>(null) }
    val scope = rememberCoroutineScope()

    LaunchedEffect(reload) {
        val records = progress.recent()
        watching = records.filter { it.resumeAtMs != null }
        listening = withContext(Dispatchers.IO) {
            store.list()
                .filter {
                    !it.finished &&
                        !it.hiddenFromHome &&
                        it.positionS >= VideoProgress.MIN_RESUME_MS / 1000.0
                }
                .map { b ->
                    Listening(
                        book = b,
                        durationS = store.chapters(b.fileId).lastOrNull()?.endS ?: 0.0,
                        lastPlayedAt = store.lastPlayedAt(b.fileId),
                    )
                }
                .sortedByDescending { it.lastPlayedAt }
        }
        loaded = true

        // Up next is the only part that asks the server, and it comes in
        // after the rest has painted rather than holding the screen for it.
        val next = mutableListOf<UpNext>()
        // Series you're watching: the newest record per show decides the
        // next episode — resume it if unfinished, else the one after it.
        val byShow = records.filter { !it.isMovie }.groupBy { it.parentId }
        shows = byShow.entries
            .sortedByDescending { (_, recs) -> recs.maxOf { it.updatedAt } }
            .mapNotNull { (sid, recs) ->
                val latest = recs.maxByOrNull { it.updatedAt } ?: return@mapNotNull null
                val s = LibraryCache.seriesDetail[sid]
                    ?: runCatching { api.series(sid) }.getOrNull()?.also { LibraryCache.seriesDetail[sid] = it }
                    ?: return@mapNotNull null
                val current = s.episodes.firstOrNull { it.id == latest.itemId }
                val target = when {
                    !latest.finished && current?.playable == true -> current
                    else -> nextEpisode(s.episodes, latest.season ?: current?.season ?: 0, latest.number ?: current?.number ?: 0)
                } ?: return@mapNotNull null
                ShowInProgress(series = s, episode = target, resuming = target.id == latest.itemId && !latest.finished)
            }
        withContext(Dispatchers.IO) {
            store.list().filter { it.finished }
                .sortedByDescending { store.lastPlayedAt(it.fileId) }
                .forEach { done ->
                    nextInSeries(store, done.fileId)?.let { nb ->
                        if (next.none { it is UpNext.Book && it.book.fileId == nb.fileId } &&
                            listening.none { it.book.fileId == nb.fileId }
                        ) {
                            next += UpNext.Book(nb, after = done)
                        }
                    }
                }
        }
        upNext = next
    }

    openShow?.let { sh ->
        BackHandler { openShow = null }
        EpisodeDetailScreen(
            series = sh.series,
            episode = sh.episode,
            api = api,
            onPlay = { playEpisode(progress, api, sh.series, sh.episode, onPlayVideo) },
            onBack = { openShow = null; reload++ },
            onOpenSeries = { openShow = null; onOpenSeries(sh.series.id, sh.episode.season) },
        )
        return
    }

    confirmForget?.let { target ->
        AlertDialog(
            onDismissRequest = { confirmForget = null },
            title = { Text("Remove from Home?") },
            text = { Text("“${target.title}” ${target.note}") },
            confirmButton = {
                TextButton(onClick = {
                    // One of these actions rewrites meta.json, so none of them run
                    // on the main thread. reload++ only after it lands, or the
                    // shelf re-reads the file it is still being written.
                    scope.launch {
                        withContext(Dispatchers.IO) { target.action() }
                        confirmForget = null
                        reload++
                    }
                }) { Text("Remove", color = MaterialTheme.colorScheme.error) }
            },
            dismissButton = { TextButton(onClick = { confirmForget = null }) { Text("Cancel") } },
        )
    }

    Column(modifier = Modifier.fillMaxSize()) {
        SkadiTopBar(title = "Skadi") {
            AccountMenu(
                onManageStorage = onManageStorage,
                onChangePassword = onChangePassword,
                onSignOut = onSignOut,
            )
        }
        // Renders nothing unless the daemon is offering a newer build.
        UpdateCard(api)
        // Above the empty-state return below on purpose: a fresh account has
        // nothing in progress, and that is exactly when the operator wants to
        // see whether anything is actually downloading.
        if (isAdmin) StatusLine(api, onOpenDownloads)

        if (!loaded) {
            Loading()
            return@Column
        }
        if (watching.isEmpty() && shows.isEmpty() && upNext.isEmpty() && listening.isEmpty()) {
            EmptyState("Nothing in progress. Start a film, an episode or a book and it will be waiting here.")
            return@Column
        }

        LazyColumn(contentPadding = PaddingValues(bottom = 24.dp)) {
            if (watching.isNotEmpty()) {
                item(key = "watching") {
                    Shelf("Continue watching") {
                        items(watching, key = { it.key }) { rec ->
                            val url = if (rec.isMovie) {
                                api.movieVideoUrl(rec.parentId, rec.itemId)
                            } else {
                                api.episodeVideoUrl(rec.parentId, rec.itemId)
                            }
                            val left = rec.durationMs.takeIf { it > 0 }?.let { it - rec.positionMs }
                            PosterCard(
                                posterUrl = rec.posterUrl,
                                title = rec.title,
                                line2 = rec.subtitle,
                                line3 = left?.let { timeLeft(it) },
                                fraction = rec.fraction,
                                onClick = {
                                    progress.start(rec)
                                    onPlayVideo(url, playerTitle(rec), rec.key)
                                },
                                onLongClick = {
                                    confirmForget = ForgetTarget(
                                        title = rec.title,
                                        note = "will start from the beginning next time.",
                                    ) { progress.forget(rec.key) }
                                },
                            )
                        }
                    }
                }
            }
            if (shows.isNotEmpty()) {
                item(key = "shows") {
                    Shelf("Series you're watching") {
                        items(shows, key = { it.series.id }) { sh ->
                            PosterCard(
                                posterUrl = sh.series.poster_url,
                                title = sh.series.title,
                                line2 = (if (sh.resuming) "Resume " else "Next: ") + sh.episode.code,
                                line3 = sh.episode.title,
                                fraction = null,
                                onClick = { openShow = sh },
                                onLongClick = null,
                            )
                        }
                    }
                }
            }
            if (upNext.isNotEmpty()) {
                item(key = "upnext") {
                    Shelf("Up next") {
                        items(upNext, key = { it.id }) { n ->
                            when (n) {
                                is UpNext.Book -> PosterCard(
                                    posterUrl = store.coverFile(n.book.fileId).takeIf { it.isFile },
                                    title = n.book.title,
                                    line2 = n.book.seriesName?.let { s ->
                                        s + (n.book.seriesPosition?.let { " #$it" } ?: "")
                                    },
                                    line3 = "after ${n.after.title}",
                                    fraction = null,
                                    square = true,
                                    onClick = { onPlayBook(n.book.fileId) },
                                    onLongClick = null,
                                )
                            }
                        }
                    }
                }
            }
            if (listening.isNotEmpty()) {
                item(key = "listening") {
                    Shelf("Keep listening") {
                        items(listening, key = { it.book.fileId }) { l ->
                            val left = l.durationS.takeIf { it > 0 }?.let { (it - l.book.positionS) * 1000 }
                            PosterCard(
                                posterUrl = store.coverFile(l.book.fileId).takeIf { it.isFile },
                                title = l.book.title,
                                line2 = l.book.authors.joinToString(", ").ifEmpty { null },
                                line3 = left?.let { timeLeft(it.toLong()) },
                                fraction = l.durationS.takeIf { it > 0 }
                                    ?.let { (l.book.positionS / it).toFloat().coerceIn(0f, 1f) },
                                square = true,
                                onClick = { onPlayBook(l.book.fileId) },
                                onLongClick = {
                                    confirmForget = ForgetTarget(
                                        title = l.book.title,
                                        // Says what survives, not just what goes.
                                        // The neighbouring gesture in the library's
                                        // Downloaded view *does* delete the file
                                        // (SKADI-T-0646), so these two must not feel
                                        // like the same action.
                                        note = "comes off Home. The download and your " +
                                            "place in it stay on your phone.",
                                    ) { store.setHiddenFromHome(l.book.fileId, true) }
                                },
                            )
                        }
                    }
                }
            }
        }
    }
}

/**
 * What the one "Remove from Home?" dialog needs, whichever shelf raised it
 * (SKADI-T-0647): a name, a line saying what survives, and the work to do.
 *
 * `note` is phrased as the second half of a sentence beginning with the title,
 * and should say what is kept, not only what is removed — the shelves differ on
 * that, and the difference matters: dismissing a video forgets the position,
 * dismissing a book keeps both the position and the download.
 */
private data class ForgetTarget(
    val title: String,
    val note: String,
    val action: () -> Unit,
)

private data class Listening(val book: OfflineBook, val durationS: Double, val lastPlayedAt: Long)

/** A show with a next unwatched episode on disk (SKADI-T-0603). */
private data class ShowInProgress(val series: Series, val episode: Episode, val resuming: Boolean)

private sealed interface UpNext {
    val id: String

    data class Book(val book: OfflineBook, val after: OfflineBook) : UpNext {
        override val id get() = "b:${book.fileId}"
    }
}

/** The first playable episode after (season, number), in broadcast order.
 *  Specials (season 0) are skipped: after S03E10 nobody wants S00E04. */
fun nextEpisode(episodes: List<Episode>, season: Int, number: Int): Episode? =
    episodes
        .filter { it.playable && it.season > 0 }
        .filter { it.season > season || (it.season == season && it.number > number) }
        .minWithOrNull(compareBy({ it.season }, { it.number }))

/** Title the video player shows: "Series · S01E03" or "Film (1999)". */
private fun playerTitle(rec: WatchRecord): String =
    if (rec.isMovie) {
        rec.subtitle?.let { "${rec.title} ($it)" } ?: rec.title
    } else {
        "${rec.title} · ${rec.subtitle?.substringBefore(" · ") ?: ""}".trimEnd(' ', '·')
    }

/** "1 h 12 min left", "42 min left", "under a minute left". */
fun timeLeft(ms: Long): String {
    val min = ms / 60_000
    return when {
        min >= 60 -> "${min / 60} h ${min % 60} min left"
        min >= 1 -> "$min min left"
        else -> "under a minute left"
    }
}

/** A titled horizontal row. */
@Composable
private fun Shelf(
    title: String,
    content: androidx.compose.foundation.lazy.LazyListScope.() -> Unit,
) {
    Column(modifier = Modifier.padding(top = 8.dp, bottom = 12.dp)) {
        Text(
            title,
            style = MaterialTheme.typography.titleMedium,
            modifier = Modifier.padding(start = 16.dp, bottom = 10.dp),
        )
        LazyRow(
            contentPadding = PaddingValues(horizontal = 16.dp),
            horizontalArrangement = Arrangement.spacedBy(12.dp),
            content = content,
        )
    }
}

/**
 * One item on a shelf: art with the progress line along its bottom edge,
 * then up to three lines of text. Posters are 2:3; book covers are square.
 */
@OptIn(ExperimentalFoundationApi::class)
@Composable
private fun PosterCard(
    posterUrl: Any?,
    title: String,
    line2: String?,
    line3: String?,
    fraction: Float?,
    onClick: () -> Unit,
    onLongClick: (() -> Unit)?,
    square: Boolean = false,
) {
    val shape = RoundedCornerShape(6.dp)
    val w = if (square) 132.dp else 120.dp
    Column(
        modifier = Modifier
            .width(w)
            .combinedClickable(onClick = onClick, onLongClick = onLongClick),
    ) {
        Box {
            val art = Modifier.fillMaxWidth().aspectRatio(if (square) 1f else 2f / 3f).clip(shape)
            if (posterUrl != null) {
                AsyncImage(
                    model = posterUrl,
                    contentDescription = null,
                    contentScale = ContentScale.Crop,
                    modifier = art,
                )
            } else {
                Box(modifier = art.background(MaterialTheme.colorScheme.surfaceVariant))
            }
            if (fraction != null) {
                LinearProgressIndicator(
                    progress = { fraction },
                    trackColor = MaterialTheme.colorScheme.onSurface.copy(alpha = 0.25f),
                    modifier = Modifier
                        .align(Alignment.BottomCenter)
                        .fillMaxWidth()
                        .height(4.dp),
                )
            }
        }
        Text(
            title,
            style = MaterialTheme.typography.bodyMedium,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
            modifier = Modifier.padding(top = 6.dp),
        )
        line2?.let {
            Text(
                it,
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
        }
        line3?.let {
            Text(
                it,
                style = MaterialTheme.typography.labelSmall,
                color = MaterialTheme.colorScheme.primary,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
        }
    }
}

/**
 * The account actions, on Home's top bar (SKADI-T-0627).
 *
 * These used to hang off the Books tab's overflow, which is where they landed
 * when Books was the only screen. Changing your password is not a books
 * action; neither is signing out. Home is the tab every role starts on, so
 * this is the one place all of them can be found.
 */
@Composable
private fun AccountMenu(
    onManageStorage: () -> Unit,
    onChangePassword: () -> Unit,
    onSignOut: () -> Unit,
) {
    var open by remember { mutableStateOf(false) }
    var confirmSignOut by remember { mutableStateOf(false) }
    Box {
        IconButton(onClick = { open = true }) {
            Icon(Icons.Filled.AccountCircle, contentDescription = "Account")
        }
        DropdownMenu(expanded = open, onDismissRequest = { open = false }) {
            DropdownMenuItem(
                text = { Text("Downloaded books") },
                onClick = { open = false; onManageStorage() },
            )
            DropdownMenuItem(
                text = { Text("Change password") },
                onClick = { open = false; onChangePassword() },
            )
            DropdownMenuItem(
                text = { Text("Sign out") },
                onClick = { open = false; confirmSignOut = true },
            )
        }
    }
    if (confirmSignOut) {
        AlertDialog(
            onDismissRequest = { confirmSignOut = false },
            title = { Text("Sign out?") },
            text = { Text("This device forgets the server and your account. You can sign back in with your username and password.") },
            confirmButton = {
                TextButton(onClick = { confirmSignOut = false; onSignOut() }) {
                    Text("Sign out", color = MaterialTheme.colorScheme.error)
                }
            },
            dismissButton = { TextButton(onClick = { confirmSignOut = false }) { Text("Cancel") } },
        )
    }
}

/**
 * "Is it downloading?" in one line (SKADI-T-0627).
 *
 * This replaces the Downloads tab, which was a whole fifth destination that
 * only the operator ever saw. The answer is almost always a number, so it is
 * a line rather than a screen; tapping it opens the full controller view.
 * Every poll is best-effort — a flaky link leaves the last numbers up rather
 * than blanking the row.
 */
@Composable
private fun StatusLine(api: SkadiApi, onOpen: () -> Unit) {
    var active by remember { mutableStateOf<Int?>(null) }
    var wanted by remember { mutableStateOf<Int?>(null) }
    var down by remember { mutableStateOf(false) }
    var allowed by remember { mutableStateOf(true) }

    PollEffect(api, 10_000L) {
        try {
            val transfers = api.listDownloads()
            active = transfers.count { it.status == "downloading" || it.status == "queued" }
            wanted = runCatching { api.wanted().summary.items }.getOrNull() ?: wanted
            down = false
        } catch (_: NotAllowedException) {
            // The role changed under us — demoted server-side, or this device's
            // cached role is stale. "Server unreachable" would be a lie and
            // would send the operator looking at the stack, so say nothing.
            allowed = false
        } catch (_: Exception) {
            down = true
        }
    }
    if (!allowed) return

    val label = when {
        down -> "Server unreachable"
        active == null -> "Checking…"
        active!! > 0 -> "$active downloading"
        wanted != null && wanted!! > 0 -> "Nothing downloading"
        else -> "All caught up"
    }
    val detail = when {
        down -> "Tap for details"
        wanted != null && wanted!! > 0 -> "${wanted} wanted"
        else -> null
    }
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .clickable(onClick = onOpen)
            .padding(horizontal = 16.dp, vertical = 12.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        Icon(
            if (down) Icons.Filled.CloudOff else Icons.Filled.Download,
            contentDescription = null,
            tint = if (down) {
                MaterialTheme.colorScheme.error
            } else {
                MaterialTheme.colorScheme.onSurfaceVariant
            },
        )
        Text(label, style = MaterialTheme.typography.bodyMedium)
        detail?.let {
            Text(
                it,
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
    }
}
