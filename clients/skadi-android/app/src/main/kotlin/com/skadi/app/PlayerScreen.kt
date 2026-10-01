package com.skadi.app

import android.content.ComponentName
import android.net.Uri
import androidx.compose.foundation.Image
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.Bedtime
import androidx.compose.material.icons.filled.Forward30
import androidx.compose.material.icons.filled.Pause
import androidx.compose.material.icons.filled.PlayArrow
import androidx.compose.material.icons.filled.Replay30
import androidx.compose.material.icons.filled.SkipNext
import androidx.compose.material.icons.filled.SkipPrevious
import androidx.compose.material3.Button
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FilterChip
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.FilledIconButton
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedIconButton
import androidx.compose.material3.Slider
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.rememberModalBottomSheetState
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.dp
import androidx.media3.common.MediaItem
import androidx.media3.common.MediaMetadata
import androidx.media3.session.MediaController
import androidx.media3.session.SessionToken
import com.google.common.util.concurrent.ListenableFuture
import com.google.common.util.concurrent.MoreExecutors
import com.skadi.core.Chapter
import com.skadi.core.OfflineStore
import com.skadi.core.SleepTimer
import kotlinx.coroutines.delay

private val SPEEDS = listOf(0.75f, 1.0f, 1.25f, 1.5f, 1.75f, 2.0f, 2.5f, 3.0f)
private val SLEEPS = listOf("Off" to 0, "15m" to 15, "30m" to 30, "45m" to 45, "60m" to 60)

private fun fmtClock(s: Long): String {
    val h = s / 3600; val m = (s % 3600) / 60; val sec = s % 60
    return if (h > 0) "%d:%02d:%02d".format(h, m, sec) else "%d:%02d".format(m, sec)
}

/** Speed label without a trailing `.0` — `1.0f` → "1", `1.5f` → "1.5". */
private fun trimSpeed(s: Float): String =
    if (s == s.toInt().toFloat()) s.toInt().toString() else s.toString()

private fun chapterAt(chapters: List<Chapter>, tS: Double): Chapter? =
    chapters.lastOrNull { it.startS <= tS }

/**
 * The player (SKADI-T-0343): controls a MediaController bound to
 * [PlaybackService]. Everything reads from on-device files — zero network.
 */
@OptIn(ExperimentalMaterial3Api::class, ExperimentalLayoutApi::class)
@Composable
fun PlayerScreen(fid: String, onBack: () -> Unit) {
    val context = LocalContext.current
    val store = remember { OfflineStore(context.filesDir) }
    val meta = remember(fid) { store.meta(fid) }
    val chapters = remember(fid) { store.chapters(fid) }
    val cover = remember(fid) {
        runCatching {
            android.graphics.BitmapFactory.decodeFile(store.coverFile(fid).absolutePath)
        }.getOrNull()
    }

    var controller by remember { mutableStateOf<MediaController?>(null) }
    var positionS by remember { mutableStateOf(meta?.positionS ?: 0.0) }
    var durationS by remember { mutableStateOf(0.0) }
    var playing by remember { mutableStateOf(false) }
    var speed by remember { mutableStateOf(1.0f) }
    // Sleep state lives in PlaybackService (survives rotation/Activity death);
    // this mirror, refreshed by the UI clock, only drives the labels and chips.
    var sleepMode by remember { mutableStateOf(PlaybackService.sleep.mode) }
    var sleepLeftMs by remember { mutableStateOf(PlaybackService.sleep.remainingMs()) }
    var showChapters by remember { mutableStateOf(false) }
    var showSheet by remember { mutableStateOf(false) }
    var showSleepSheet by remember { mutableStateOf(false) }
    val playerSettings = remember { com.skadi.core.PlayerSettings(context) }
    var defaultSpeed by remember { mutableStateOf(playerSettings.defaultSpeed) }
    var shakeToExtend by remember { mutableStateOf(playerSettings.shakeToExtend) }

    // Bind the controller; load the book if the service isn't already on it.
    // Released via releaseFuture (review pass 2, A10): releasing through the
    // state var leaked the controller when unmounting before the async build
    // completed.
    DisposableEffect(fid) {
        val token = SessionToken(context, ComponentName(context, PlaybackService::class.java))
        val future: ListenableFuture<MediaController> = MediaController.Builder(context, token).buildAsync()
        future.addListener({
            val c = future.get()
            controller = c
            if (c.currentMediaItem?.mediaId != fid) {
                val item = MediaItem.Builder()
                    .setMediaId(fid)
                    .setUri(Uri.fromFile(store.audioFile(fid)))
                    .setMediaMetadata(
                        MediaMetadata.Builder()
                            .setTitle(meta?.title ?: "Skadi")
                            .setArtist(meta?.authors?.joinToString(", ") ?: "")
                            .setArtworkUri(
                                store.coverFile(fid).takeIf { it.isFile }?.let(Uri::fromFile),
                            )
                            .build(),
                    )
                    .build()
                c.setMediaItem(item, ((meta?.positionS ?: 0.0) * 1000).toLong())
                // The book's own speed, else the default (SKADI-T-0659). It
                // used to carry over whatever the last book was played at.
                c.setPlaybackSpeed(playerSettings.speedFor(meta))
                c.prepare()
                // The tap that got here said "Play" (a library row, a Home
                // card), so play. Opening paused meant every resume was two
                // taps, which is the opposite of picking up where you left
                // off (SKADI-T-0578). Reopening the book already loaded (from
                // the mini-player) leaves its state alone.
                c.play()
            }
            speed = c.playbackParameters.speed
        }, MoreExecutors.directExecutor())
        onDispose {
            MediaController.releaseFuture(future)
            controller = null
        }
    }

    // UI clock + sleep enforcement.
    LaunchedEffect(controller) {
        while (true) {
            controller?.let { c ->
                positionS = c.currentPosition / 1000.0
                durationS = (c.duration.takeIf { it > 0 } ?: 0L) / 1000.0
                playing = c.isPlaying
            }
            // Service enforces the sleep timer; mirror its state for the label.
            sleepMode = PlaybackService.sleep.mode
            sleepLeftMs = PlaybackService.sleep.remainingMs()
            delay(500)
        }
    }

    val next = remember(fid) { nextInSeries(store, fid) }

    Column(
        modifier = Modifier.fillMaxSize().padding(20.dp),
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        Row(modifier = Modifier.fillMaxWidth()) {
            androidx.compose.material3.IconButton(onClick = onBack) {
                androidx.compose.material3.Icon(
                    androidx.compose.material.icons.Icons.AutoMirrored.Filled.ArrowBack,
                    contentDescription = "Back",
                    tint = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        }
        cover?.let {
            Image(
                bitmap = it.asImageBitmap(),
                contentDescription = null,
                modifier = Modifier.size(240.dp).clip(RoundedCornerShape(12.dp)),
            )
        }
        Text(meta?.title ?: "", style = MaterialTheme.typography.headlineSmall, modifier = Modifier.padding(top = 12.dp))
        Text(
            listOfNotNull(
                meta?.authors?.joinToString(", ")?.ifEmpty { null },
                meta?.seriesName?.let { n -> n + (meta.seriesPosition?.let { " #$it" } ?: "") },
            ).joinToString(" · "),
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            style = MaterialTheme.typography.bodySmall,
        )
        // Chapter title + a CHAPTER-SCOPED progress bar: the slider tracks the
        // position WITHIN the current chapter, not the whole book — audiobook
        // listeners think per-chapter (SKADI-I-0052). Falls back to whole-book
        // when the file carries no chapters.
        val curChapter = chapterAt(chapters, positionS)
        Text(
            curChapter?.let { it.title.ifBlank { "Chapter ${it.index + 1}" } } ?: "",
            color = MaterialTheme.colorScheme.primary,
            style = MaterialTheme.typography.bodyMedium,
            modifier = Modifier.padding(top = 8.dp),
        )
        // Seek on RELEASE only — per-delta seeks in a multi-hour m4b are a
        // stutter storm (review pass 2, A12). Scrub range is the current chapter's
        // [startS, endS] (absolute book seconds), so a release seeks in-chapter.
        var scrub by remember { mutableStateOf<Float?>(null) }
        val barLo = curChapter?.startS?.toFloat() ?: 0f
        val barHi = (curChapter?.endS?.toFloat() ?: durationS.toFloat()).coerceAtLeast(barLo + 1f)
        Slider(
            value = scrub ?: positionS.toFloat().coerceIn(barLo, barHi),
            onValueChange = { v -> scrub = v },
            onValueChangeFinished = {
                scrub?.let { controller?.seekTo((it * 1000).toLong()) }
                scrub = null
            },
            valueRange = barLo..barHi,
            modifier = Modifier.fillMaxWidth().padding(top = 8.dp),
        )
        // Elapsed within the chapter · chapter length (not book-level times).
        Row(modifier = Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.SpaceBetween) {
            val base = curChapter?.startS ?: 0.0
            val end = curChapter?.endS ?: durationS
            Text(fmtClock((positionS - base).coerceAtLeast(0.0).toLong()), color = MaterialTheme.colorScheme.onSurfaceVariant, style = MaterialTheme.typography.bodySmall)
            Text(fmtClock((end - base).coerceAtLeast(0.0).toLong()), color = MaterialTheme.colorScheme.onSurfaceVariant, style = MaterialTheme.typography.bodySmall)
        }
        // Transport: fixed-size circular icon buttons in a full-width evenly
        // spaced row — always fits a phone (the old min-width buttons clipped
        // the ⏭ off the right edge).
        Row(
            horizontalArrangement = Arrangement.SpaceEvenly,
            verticalAlignment = Alignment.CenterVertically,
            modifier = Modifier.fillMaxWidth().padding(top = 12.dp),
        ) {
            OutlinedIconButton(
                onClick = {
                    val target = chapterAt(chapters, positionS)?.let { ch ->
                        if (positionS - ch.startS > 3.0) ch.startS
                        else chapters.lastOrNull { it.startS < ch.startS }?.startS ?: 0.0
                    } ?: 0.0
                    controller?.seekTo((target * 1000).toLong())
                },
                enabled = chapters.isNotEmpty(),
                modifier = Modifier.size(48.dp),
            ) { Icon(Icons.Filled.SkipPrevious, contentDescription = "Previous chapter") }
            OutlinedIconButton(
                onClick = { controller?.let { it.seekTo(it.currentPosition - 30_000) } },
                modifier = Modifier.size(52.dp),
            ) { Icon(Icons.Filled.Replay30, contentDescription = "Back 30 seconds") }
            FilledIconButton(
                onClick = { controller?.let { if (it.isPlaying) it.pause() else it.play() } },
                modifier = Modifier.size(72.dp),
            ) {
                Icon(
                    if (playing) Icons.Filled.Pause else Icons.Filled.PlayArrow,
                    contentDescription = if (playing) "Pause" else "Play",
                    modifier = Modifier.size(36.dp),
                )
            }
            OutlinedIconButton(
                onClick = { controller?.let { it.seekTo(it.currentPosition + 30_000) } },
                modifier = Modifier.size(52.dp),
            ) { Icon(Icons.Filled.Forward30, contentDescription = "Forward 30 seconds") }
            OutlinedIconButton(
                onClick = {
                    chapterAt(chapters, positionS)?.let { ch ->
                        chapters.firstOrNull { it.startS > ch.startS }
                            ?.let { controller?.seekTo((it.startS * 1000).toLong()) }
                    }
                },
                enabled = chapters.isNotEmpty(),
                modifier = Modifier.size(48.dp),
            ) { Icon(Icons.Filled.SkipNext, contentDescription = "Next chapter") }
        }
        Row(
            horizontalArrangement = Arrangement.spacedBy(10.dp),
            modifier = Modifier.padding(top = 14.dp),
        ) {
            OutlinedButton(onClick = { showSheet = true }) {
                Text("${trimSpeed(speed)}×")
            }
            // Its own button (SKADI-T-0658): it used to share the speed
            // button, labelled "1×", and could not be found. Counts down live
            // while armed (SKADI-T-0657).
            OutlinedButton(onClick = { showSleepSheet = true }) {
                Icon(
                    Icons.Filled.Bedtime,
                    contentDescription = null,
                    modifier = Modifier.size(18.dp).padding(end = 4.dp),
                )
                Text(
                    when (sleepMode) {
                        null -> "Sleep"
                        is SleepTimer.Mode.Minutes -> SleepTimer.clock(sleepLeftMs ?: 0L)
                        SleepTimer.Mode.EndOfChapter -> "End of chapter"
                    },
                )
            }
            OutlinedButton(onClick = { showChapters = !showChapters }, enabled = chapters.isNotEmpty()) {
                Text("Chapters")
            }
        }
        if (showSheet) {
            val sheetState = rememberModalBottomSheetState()
            ModalBottomSheet(onDismissRequest = { showSheet = false }, sheetState = sheetState) {
                Column(
                    modifier = Modifier
                        .fillMaxWidth()
                        .padding(horizontal = 20.dp)
                        .padding(bottom = 28.dp),
                ) {
                    Text("Playback speed", style = MaterialTheme.typography.titleMedium)
                    FlowRow(
                        horizontalArrangement = Arrangement.spacedBy(8.dp),
                        modifier = Modifier.padding(top = 8.dp),
                    ) {
                        SPEEDS.forEach { s ->
                            FilterChip(
                                selected = kotlin.math.abs(s - speed) < 0.01f,
                                onClick = {
                                    speed = s
                                    controller?.setPlaybackSpeed(s)
                                    // This book's speed from now on (SKADI-T-0659).
                                    // Saved here, not on the player's speed
                                    // change: opening a book applies the
                                    // default, and that must not become the
                                    // book's own.
                                    store.updateSpeed(fid, s)
                                },
                                label = { Text("${trimSpeed(s)}×") },
                            )
                        }
                    }
                    Row(
                        verticalAlignment = Alignment.CenterVertically,
                        modifier = Modifier.fillMaxWidth().padding(top = 12.dp),
                    ) {
                        Text(
                            "Default for other books: ${trimSpeed(defaultSpeed)}×",
                            style = MaterialTheme.typography.bodyMedium,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                            modifier = Modifier.weight(1f),
                        )
                        TextButton(
                            onClick = {
                                playerSettings.defaultSpeed = speed
                                defaultSpeed = speed
                            },
                            enabled = kotlin.math.abs(speed - defaultSpeed) >= 0.01f,
                        ) { Text("Make ${trimSpeed(speed)}× the default") }
                    }
                }
            }
        }
        if (showSleepSheet) {
            ModalBottomSheet(
                onDismissRequest = { showSleepSheet = false },
                sheetState = rememberModalBottomSheetState(),
            ) {
                Column(
                    modifier = Modifier
                        .fillMaxWidth()
                        .padding(horizontal = 20.dp)
                        .padding(bottom = 28.dp),
                ) {
                    Text("Sleep timer", style = MaterialTheme.typography.titleMedium)
                    FlowRow(
                        horizontalArrangement = Arrangement.spacedBy(8.dp),
                        modifier = Modifier.padding(top = 8.dp),
                    ) {
                        SLEEPS.forEach { (label, mins) ->
                            val selected = when (val m = sleepMode) {
                                null -> mins == 0
                                is SleepTimer.Mode.Minutes -> m.totalMs == mins * 60_000L
                                SleepTimer.Mode.EndOfChapter -> false
                            }
                            FilterChip(
                                selected = selected,
                                onClick = {
                                    if (mins == 0) PlaybackService.cancelSleep()
                                    else PlaybackService.armSleepMinutes(mins)
                                    sleepMode = PlaybackService.sleep.mode
                                    sleepLeftMs = PlaybackService.sleep.remainingMs()
                                },
                                label = { Text(label) },
                            )
                        }
                        FilterChip(
                            selected = sleepMode == SleepTimer.Mode.EndOfChapter,
                            onClick = {
                                PlaybackService.armSleepEndOfChapter()
                                sleepMode = PlaybackService.sleep.mode
                                sleepLeftMs = null
                            },
                            label = { Text("End of chapter") },
                            enabled = chapters.isNotEmpty(),
                        )
                    }
                    Row(
                        verticalAlignment = Alignment.CenterVertically,
                        modifier = Modifier.fillMaxWidth().padding(top = 18.dp),
                    ) {
                        Column(modifier = Modifier.weight(1f)) {
                            Text("Shake to extend", style = MaterialTheme.typography.bodyLarge)
                            Text(
                                "In the last minute, shake the phone to start the timer again.",
                                style = MaterialTheme.typography.bodySmall,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                            )
                        }
                        Switch(
                            checked = shakeToExtend,
                            onCheckedChange = {
                                shakeToExtend = it
                                playerSettings.shakeToExtend = it
                            },
                        )
                    }
                }
            }
        }
        next?.let { n ->
            if (meta?.finished == true || durationS > 0 && positionS / durationS >= 0.98) {
                Spacer(modifier = Modifier.height(10.dp))
                Text("Up next in ${n.seriesName}: ${n.title}", color = MaterialTheme.colorScheme.secondary, style = MaterialTheme.typography.bodySmall)
            }
        }
        if (showChapters) {
            LazyColumn(modifier = Modifier.fillMaxWidth().padding(top = 10.dp)) {
                items(chapters, key = { it.index }) { ch ->
                    val cur = chapterAt(chapters, positionS)?.index == ch.index
                    Row(
                        modifier = Modifier.fillMaxWidth().padding(vertical = 6.dp),
                        horizontalArrangement = Arrangement.SpaceBetween,
                    ) {
                        TextButton(onClick = { controller?.seekTo((ch.startS * 1000).toLong()) }) {
                            Text(
                                ch.title.ifBlank { "Chapter ${ch.index + 1}" },
                                color = if (cur) MaterialTheme.colorScheme.primary else Color.Unspecified,
                            )
                        }
                        // The chapter's LENGTH, not its offset into the book.
                        Text(
                            fmtClock((ch.endS - ch.startS).coerceAtLeast(0.0).toLong()),
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                            style = MaterialTheme.typography.bodySmall,
                        )
                    }
                }
            }
        }
    }
}

/** The next unfinished book in the same series among the downloads. */
fun nextInSeries(store: OfflineStore, fid: String): com.skadi.core.OfflineBook? {
    val cur = store.meta(fid) ?: return null
    val series = cur.seriesName ?: return null
    val curPos = cur.seriesPosition?.toDoubleOrNull() ?: return null
    return store.list()
        .filter { it.seriesName == series && !it.finished && it.fileId != fid }
        .mapNotNull { b -> b.seriesPosition?.toDoubleOrNull()?.let { it to b } }
        .filter { (p, _) -> p > curPos }
        .minByOrNull { (p, _) -> p }
        ?.second
}
