package com.skadi.app

import android.content.ComponentName
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Pause
import androidx.compose.material.icons.filled.PlayArrow
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.media3.common.Player
import androidx.media3.session.MediaController
import androidx.media3.session.SessionToken
import coil.compose.AsyncImage
import com.google.common.util.concurrent.ListenableFuture
import com.google.common.util.concurrent.MoreExecutors
import com.skadi.core.OfflineStore
import kotlinx.coroutines.delay

/**
 * Persistent now-playing bar (SKADI-T-0350): the single biggest gap vs every
 * audiobook app — once you leave the player the audio was invisible. This binds
 * its own [MediaController] to [PlaybackService], shows the current book +
 * play/pause + a progress line, and taps through to the full player. Renders
 * nothing when nothing is loaded.
 *
 * Docked, not floating (SKADI-T-0577): it sits full-width directly above the
 * navigation bar as part of the frame, with the progress line along its top
 * edge. The old version was a rounded card with 16 dp of margin and `⏸`/`▶`
 * text glyphs, which read as one more list item rather than a control.
 */
@Composable
fun MiniPlayerBar(onOpen: (String) -> Unit) {
    val context = LocalContext.current
    val store = remember { OfflineStore(context.filesDir) }
    var controller by remember { mutableStateOf<MediaController?>(null) }
    var fid by remember { mutableStateOf<String?>(null) }
    var playing by remember { mutableStateOf(false) }
    var progress by remember { mutableStateOf(0f) }
    // Armed sleep timer, so it is visible outside the player (SKADI-T-0658).
    var sleepStatus by remember { mutableStateOf<String?>(null) }

    DisposableEffect(Unit) {
        val token = SessionToken(context, ComponentName(context, PlaybackService::class.java))
        val future: ListenableFuture<MediaController> =
            MediaController.Builder(context, token).buildAsync()
        future.addListener({
            val c = future.get()
            controller = c
            fid = c.currentMediaItem?.mediaId
            playing = c.isPlaying
            c.addListener(object : Player.Listener {
                override fun onIsPlayingChanged(isPlaying: Boolean) { playing = isPlaying }
                override fun onMediaItemTransition(item: androidx.media3.common.MediaItem?, reason: Int) {
                    fid = item?.mediaId
                }
            })
        }, MoreExecutors.directExecutor())
        onDispose { MediaController.releaseFuture(future); controller = null }
    }

    // Poll position for the thin progress line.
    LaunchedEffect(controller) {
        while (true) {
            controller?.let { c ->
                fid = c.currentMediaItem?.mediaId
                val dur = c.duration.takeIf { it > 0 } ?: 0L
                progress = if (dur > 0) (c.currentPosition.toFloat() / dur).coerceIn(0f, 1f) else 0f
                playing = c.isPlaying
            }
            sleepStatus = com.skadi.core.SleepTimer.statusLabel(
                PlaybackService.sleep.mode,
                PlaybackService.sleep.remainingMs(),
            )
            delay(1000)
        }
    }

    val currentFid = fid ?: return
    val meta = remember(currentFid) { store.meta(currentFid) }

    Column(
        modifier = Modifier
            .fillMaxWidth()
            .background(MaterialTheme.colorScheme.surfaceContainer)
            .clickable { onOpen(currentFid) },
    ) {
        LinearProgressIndicator(
            progress = { progress },
            trackColor = MaterialTheme.colorScheme.surfaceVariant,
            modifier = Modifier.fillMaxWidth().height(2.dp),
        )
        Row(
            modifier = Modifier.fillMaxWidth().padding(start = 12.dp, end = 4.dp, top = 6.dp, bottom = 6.dp),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            val cover = store.coverFile(currentFid).takeIf { it.isFile }
            if (cover != null) {
                AsyncImage(
                    model = cover,
                    contentDescription = null,
                    contentScale = ContentScale.Crop,
                    modifier = Modifier.size(40.dp).clip(RoundedCornerShape(4.dp)),
                )
            }
            Column(modifier = Modifier.weight(1f)) {
                Text(
                    meta?.title ?: "Playing",
                    style = MaterialTheme.typography.bodyMedium,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                    color = MaterialTheme.colorScheme.onSurface,
                )
                listOfNotNull(
                    meta?.authors?.joinToString(", ")?.takeIf { it.isNotEmpty() },
                    sleepStatus,
                ).joinToString(" · ").takeIf { it.isNotEmpty() }?.let {
                    Text(
                        it,
                        style = MaterialTheme.typography.bodySmall,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
            }
            IconButton(onClick = {
                controller?.let { if (it.isPlaying) it.pause() else it.play() }
            }) {
                Icon(
                    if (playing) Icons.Filled.Pause else Icons.Filled.PlayArrow,
                    contentDescription = if (playing) "Pause" else "Play",
                    tint = MaterialTheme.colorScheme.onSurface,
                )
            }
        }
    }
}
