package com.skadi.app

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.aspectRatio
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.ui.layout.layout
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Download
import androidx.compose.material.icons.filled.PlayArrow
import androidx.compose.material3.AssistChip
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.FilledTonalButton
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import coil.compose.AsyncImage
import kotlinx.coroutines.launch
import com.skadi.core.Book
import com.skadi.core.Episode
import com.skadi.core.Movie
import com.skadi.core.Series
import com.skadi.core.SkadiApi
import com.skadi.core.Acquirable
import com.skadi.core.VideoProgress

/**
 * Info pages (SKADI-T-0586): what a tile opens *before* anything plays.
 *
 * Tapping a poster used to start the file. That is the right thing on Home,
 * where you are resuming something you chose already, and the wrong thing in
 * a library of 1,800 films, where the tap is a question — what is this, how
 * long is it, is it on disk, where was I — and only sometimes a decision.
 * One layout serves all four kinds: art, a line of facts, one primary action
 * that says exactly what it will do, and the blurb.
 */
@Composable
private fun DetailHeader(
    backdrop: String?,
    poster: String?,
    title: String,
    facts: List<String>,
    square: Boolean = false,
    onBack: () -> Unit,
) {
    // Backdrop where there is one, else the poster blurred would be nicer but
    // costs a library; a flat surface reads fine and keeps the layout stable.
    Box(modifier = Modifier.fillMaxWidth()) {
        if (backdrop != null) {
            AsyncImage(
                model = backdrop,
                contentDescription = null,
                contentScale = ContentScale.Crop,
                modifier = Modifier.fillMaxWidth().aspectRatio(16f / 9f),
            )
            Box(
                modifier = Modifier
                    .fillMaxWidth()
                    .aspectRatio(16f / 9f)
                    .background(
                        Brush.verticalGradient(
                            0f to Color.Transparent,
                            0.55f to MaterialTheme.colorScheme.background.copy(alpha = 0.35f),
                            1f to MaterialTheme.colorScheme.background,
                        ),
                    ),
            )
        } else {
            Spacer(modifier = Modifier.fillMaxWidth().height(96.dp))
        }
        SkadiTopBar(title = "", onBack = onBack)
    }
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .padding(horizontal = 16.dp)
            .pullUp(if (backdrop != null) 56.dp else 40.dp),
        verticalAlignment = Alignment.Bottom,
    ) {
        val shape = RoundedCornerShape(6.dp)
        val w = 110.dp
        if (poster != null) {
            AsyncImage(
                model = poster,
                contentDescription = null,
                contentScale = ContentScale.Crop,
                modifier = Modifier.width(w).aspectRatio(if (square) 1f else 2f / 3f).clip(shape),
            )
        } else {
            ArtPlaceholder(Modifier.width(w), shape, poster = !square)
        }
        Column(modifier = Modifier.weight(1f).padding(start = 16.dp, bottom = 4.dp)) {
            Text(
                title,
                style = MaterialTheme.typography.headlineSmall,
                maxLines = 3,
                overflow = TextOverflow.Ellipsis,
            )
            if (facts.isNotEmpty()) {
                Text(
                    facts.joinToString(" · "),
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.padding(top = 4.dp),
                )
            }
        }
    }
}

/**
 * Draw `by` higher than laid out AND give the space back, so what follows
 * moves up with it. A plain `offset` leaves the original slot empty, which
 * showed as a blank band under every header.
 */
private fun Modifier.pullUp(by: androidx.compose.ui.unit.Dp): Modifier = layout { measurable, constraints ->
    val placeable = measurable.measure(constraints)
    val shift = by.roundToPx()
    layout(placeable.width, (placeable.height - shift).coerceAtLeast(0)) {
        placeable.placeRelative(0, -shift)
    }
}

/** "1 h 47 min" from minutes. */
fun runtimeText(minutes: Int?): String? = minutes?.takeIf { it > 0 }?.let {
    if (it >= 60) "${it / 60} h ${it % 60} min" else "$it min"
}

/** Publisher blurbs arrive as HTML; the phone shows text. */
fun plainText(html: String?): String? =
    html?.let { android.text.Html.fromHtml(it, android.text.Html.FROM_HTML_MODE_LEGACY).toString() }
        ?.replace(Regex("\\n{3,}"), "\n\n")?.trim()
        ?.takeIf { it.isNotEmpty() }

/** The primary action row: one filled button saying what will happen. */
@Composable
private fun PrimaryAction(label: String, icon: androidx.compose.ui.graphics.vector.ImageVector, enabled: Boolean, onClick: () -> Unit) {
    Button(
        onClick = onClick,
        enabled = enabled,
        modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp),
        colors = ButtonDefaults.buttonColors(
            containerColor = MaterialTheme.colorScheme.primary,
            contentColor = MaterialTheme.colorScheme.onPrimary,
        ),
    ) {
        Icon(icon, contentDescription = null, modifier = Modifier.padding(end = 8.dp))
        Text(label, style = MaterialTheme.typography.titleMedium)
    }
}

@Composable
private fun Overview(text: String?) {
    val body = text ?: return
    var expanded by remember { mutableStateOf(false) }
    Text(
        body,
        style = MaterialTheme.typography.bodyMedium,
        color = MaterialTheme.colorScheme.onSurface,
        maxLines = if (expanded) Int.MAX_VALUE else 6,
        overflow = TextOverflow.Ellipsis,
        modifier = Modifier
            .padding(horizontal = 16.dp, vertical = 12.dp)
            .clickable { expanded = !expanded },
    )
}

/** "Resume · 41 min left" when a position is saved, else the plain label. */
private fun playLabel(context: android.content.Context, key: String, plain: String): String {
    val rec = VideoProgress(context).get(key) ?: return plain
    val at = rec.resumeAtMs ?: return plain
    val left = rec.durationMs.takeIf { it > 0 }?.let { timeLeft(it - at) }
    return if (left != null) "Resume · $left" else "Resume"
}

@Composable
fun MovieDetailScreen(
    movie: Movie,
    api: SkadiApi,
    onPlay: (editionId: String) -> Unit,
    onBack: () -> Unit,
) {
    val context = LocalContext.current
    val eid = movie.playableEditionId
    Column(modifier = Modifier.fillMaxSize().verticalScroll(rememberScrollState())) {
        DetailHeader(
            backdrop = movie.backdrop_url,
            poster = movie.poster_url,
            title = movie.title,
            facts = listOfNotNull(movie.year?.toString(), movie.content_rating, runtimeText(movie.runtime_minutes), movie.genres.take(3).joinToString(", ").ifEmpty { null }),
            onBack = onBack,
        )
        if (eid != null) {
            PrimaryAction(
                label = playLabel(context, "movie:${movie.id}:$eid", "Play"),
                icon = Icons.Filled.PlayArrow,
                enabled = true,
                onClick = { onPlay(eid) },
            )
        } else {
            PrimaryAction(label = "Not on disk", icon = Icons.Filled.PlayArrow, enabled = false, onClick = {})
        }
        // Hunting (SKADI-T-0602): search, pick a release, paste a link, or
        // stop looking. Shown for an owned movie too — an upgrade is a hunt.
        movie.editions.firstOrNull()?.let { ed ->
            HuntPanel(
                api = api,
                path = Acquirable.movieEdition(movie.id, ed.id),
                what = movie.displayTitle,
                monitored = movie.monitored,
                onSetMonitored = { api.setMovieMonitored(movie.id, it) },
            )
        }
        movie.collection?.let {
            Text(
                "Part of ${it.name}",
                style = MaterialTheme.typography.labelLarge,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.padding(horizontal = 16.dp, vertical = 6.dp),
            )
        }
        Overview(movie.overview)
    }
}

@Composable
fun BookDetailScreen(
    book: Book,
    api: SkadiApi,
    downloaded: Boolean,
    /** 0..1 while a download runs. */
    downloading: Float?,
    onPlay: (fileId: String) -> Unit,
    onDownload: (fileId: String) -> Unit,
    onBack: () -> Unit,
    /** Tap on a narrator chip: the library filtered to that narrator (SKADI-T-0604). */
    onNarrator: ((String) -> Unit)? = null,
) {
    val fid = book.importedFileId
    Column(modifier = Modifier.fillMaxSize().verticalScroll(rememberScrollState())) {
        DetailHeader(
            backdrop = null,
            poster = book.coverUrl?.takeIf { it.isNotEmpty() },
            title = book.title,
            facts = listOfNotNull(
                book.authors.joinToString(", ").ifEmpty { null },
                book.year?.toString(),
                runtimeText(book.runtimeMinutes),
            ),
            square = true,
            onBack = onBack,
        )
        when {
            fid == null -> PrimaryAction("Not on disk", Icons.Filled.Download, enabled = false) {}
            downloaded -> PrimaryAction("Listen", Icons.Filled.PlayArrow, enabled = true) { onPlay(fid) }
            downloading != null -> PrimaryAction(
                "Downloading · ${(downloading * 100).toInt()}%",
                Icons.Filled.Download,
                enabled = false,
            ) {}
            else -> PrimaryAction("Download to listen offline", Icons.Filled.Download, enabled = true) { onDownload(fid) }
        }
        book.files.firstOrNull()?.let { f ->
            HuntPanel(
                api = api,
                path = Acquirable.bookFile(book.id, f.id),
                what = book.title,
                monitored = book.monitored,
                onSetMonitored = { api.setBookMonitored(book.id, it) },
            )
        }
        if (book.narrators.isNotEmpty()) {
            Row(
                horizontalArrangement = Arrangement.spacedBy(8.dp),
                verticalAlignment = Alignment.CenterVertically,
                modifier = Modifier.horizontalScroll(rememberScrollState()).padding(horizontal = 16.dp, vertical = 4.dp),
            ) {
                Text("Read by", style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.onSurfaceVariant)
                book.narrators.forEach { n ->
                    AssistChip(onClick = { onNarrator?.invoke(n) }, enabled = onNarrator != null, label = { Text(n) })
                }
            }
        }
        val meta = listOfNotNull(
            book.series?.let { s -> s.name + (s.position?.let { " #$it" } ?: "") },
        )
        meta.forEach {
            Text(
                it,
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.padding(horizontal = 16.dp, vertical = 4.dp),
            )
        }
        Overview(plainText(book.overview))
    }
}

/** The header of a series page: art, facts, blurb. The season picker and
 *  episodes follow it on the series screen. */
@Composable
fun SeriesHeader(series: Series, onBack: () -> Unit) {
    DetailHeader(
        backdrop = series.backdrop_url,
        poster = series.poster_url,
        title = series.title,
        facts = listOfNotNull(
            series.year?.toString(),
            series.content_rating,
            series.network,
            series.status?.takeIf { it.isNotBlank() },
            series.genres.take(3).joinToString(", ").ifEmpty { null },
            series.playableCount.takeIf { it > 0 }?.let { "$it episodes on disk" },
        ),
        onBack = onBack,
    )
    Overview(series.overview)
}

@Composable
fun EpisodeDetailScreen(
    series: Series,
    episode: Episode,
    api: SkadiApi,
    onPlay: () -> Unit,
    onBack: () -> Unit,
    /** Up to the show, at this episode's season (SKADI-T-0608). */
    onOpenSeries: (() -> Unit)? = null,
) {
    val context = LocalContext.current
    Column(modifier = Modifier.fillMaxSize().verticalScroll(rememberScrollState())) {
        DetailHeader(
            backdrop = series.backdrop_url,
            poster = series.poster_url,
            title = episode.title ?: "Episode ${episode.number}",
            facts = listOfNotNull(
                series.title,
                episode.code,
                episode.air_date,
                runtimeText(series.runtime_minutes),
            ),
            onBack = onBack,
        )
        if (episode.playable) {
            PrimaryAction(
                label = playLabel(context, "episode:${series.id}:${episode.id}", "Play"),
                icon = Icons.Filled.PlayArrow,
                enabled = true,
                onClick = onPlay,
            )
        } else {
            PrimaryAction("Not on disk", Icons.Filled.PlayArrow, enabled = false) {}
        }
        if (onOpenSeries != null) {
            TextButton(onClick = onOpenSeries, modifier = Modifier.padding(horizontal = 8.dp)) {
                Text("Open ${series.title} · Season ${episode.season}")
            }
        }
        HuntPanel(
            api = api,
            path = Acquirable.episode(series.id, episode.id),
            what = "${series.title} · ${episode.code}",
            monitored = episode.monitored,
            onSetMonitored = { api.monitorEpisode(series.id, episode.id, it) },
        ) { notify ->
            // Whole-season switch beside the episode one: a season nobody wants
            // is the usual reason to be on this page at all.
            val scope = rememberCoroutineScope()
            val seasonOn = series.seasons.firstOrNull { it.number == episode.season }?.monitored ?: true
            var on by remember(episode.season) { mutableStateOf(seasonOn) }
            TextButton(onClick = {
                val next = !on
                scope.launch {
                    runCatching { api.monitorSeason(series.id, episode.season, next) }
                        .onSuccess { on = next; notify(if (next) "Season ${episode.season} monitored." else "Season ${episode.season} unmonitored.") }
                        .onFailure { notify(it.message ?: "Couldn't change the season.") }
                }
            }) { Text(if (on) "Unmonitor season ${episode.season}" else "Monitor season ${episode.season}") }
        }
        // Episode synopses are not in the API; the series blurb is the next
        // best thing and better than a blank page.
        Overview(series.overview)
    }
}
