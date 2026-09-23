package com.skadi.app

import android.app.Activity
import android.content.pm.ActivityInfo
import android.view.WindowManager
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.ArrowBack
import androidx.compose.material.icons.filled.SkipNext
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
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.dp
import androidx.compose.ui.viewinterop.AndroidView
import androidx.media3.common.MediaItem
import androidx.media3.common.PlaybackException
import androidx.media3.common.Player
import androidx.media3.exoplayer.ExoPlayer
import androidx.media3.ui.PlayerView

/**
 * Full-screen video playback (SKADI-T-0575).
 *
 * **Direct play.** skadi has no encoder, so the device decodes whatever is on
 * disk. Measured over this library that is ~98 % viable — HEVC/H.264 with
 * AAC/AC3/E-AC3 — but the residue (DTS) will fail, so the error path below is
 * not decoration: without it an unsupported track is a black screen with no
 * explanation, which is the worst possible outcome for a media player.
 *
 * Its own [ExoPlayer] rather than [PlaybackService]'s: that service is an audio
 * MediaSession with a notification and a sleep timer, none of which a video
 * surface wants, and sharing one player would mean video playback silently
 * inheriting the audiobook's sleep deadline.
 */
@androidx.annotation.OptIn(androidx.media3.common.util.UnstableApi::class)
@Composable
fun VideoPlayerScreen(
    url: String,
    title: String,
    /** Stable id for resume (SKADI-T-0585); see [com.skadi.core.VideoProgress]. */
    progressKey: String,
    onBack: () -> Unit,
    /** Episode code of what follows, when there is one — drawn as a button. */
    nextLabel: String? = null,
    /** Advance: called at the end of the file and from the Next button. */
    onNext: (() -> Unit)? = null,
) {
    // The listener below is created once; without this it would keep the
    // `onNext` that existed at creation — usually the `null` from before the
    // next episode had been resolved.
    val onNextNow = androidx.compose.runtime.rememberUpdatedState(onNext)
    val context = LocalContext.current
    var error by remember { mutableStateOf<String?>(null) }
    // A decode failure is permanent for this device — offering Retry on it would
    // be a button that cannot work. A network failure is exactly the opposite.
    var retryable by remember { mutableStateOf(false) }
    val progress = remember { com.skadi.core.VideoProgress(context) }
    // Read ONCE, before the player exists. Reading it later would race the
    // player's own position updates and resume to wherever playback had already
    // got to — which is to say, nowhere.
    val resumeAt = remember(progressKey) { progress.resumeAt(progressKey) }

    val player = remember {
        ExoPlayer.Builder(context)
            // Request audio focus, so starting a film pauses the audiobook
            // (SKADI-T-0577). Without this the video player never *asked* for
            // focus, so both played at once — [PlaybackService] already sets
            // `handleAudioFocus = true` and therefore respects a request; there
            // simply was not one.
            //
            // Done through audio focus rather than by reaching into
            // PlaybackService directly, because the OS is what mediates this:
            // the same request also pauses Spotify, a podcast app, or anything
            // else holding focus, and hands focus back when the film ends.
            //
            // `CONTENT_TYPE_MOVIE` where the audiobook uses `SPEECH` — that is
            // what it is, and the distinction drives per-app volume and ducking
            // behaviour on some devices.
            .setAudioAttributes(
                androidx.media3.common.AudioAttributes.Builder()
                    .setUsage(androidx.media3.common.C.USAGE_MEDIA)
                    .setContentType(androidx.media3.common.C.AUDIO_CONTENT_TYPE_MOVIE)
                    .build(),
                /* handleAudioFocus = */ true,
            )
            // Keep the CPU alive while streaming, as the audio service does — a
            // film paused by doze mid-stream is the same failure SKADI-T-0358
            // fixed for downloads.
            .setWakeMode(androidx.media3.common.C.WAKE_MODE_NETWORK)
            // Unplugging headphones pauses rather than blaring from the speaker.
            .setHandleAudioBecomingNoisy(true)
            .build().apply {
            setMediaItem(MediaItem.fromUri(url))
            // Seek BEFORE prepare: media3 applies a pending seek as the start
            // position, so the film opens where you left it instead of playing a
            // second from the top and jumping.
            resumeAt?.let { seekTo(it) }
            prepare()
            playWhenReady = true
        }
    }

    // Persist position while playing (SKADI-T-0585). Every 10s, matching the
    // audio service's cadence — often enough that a crash or a battery death
    // loses seconds rather than an hour, rare enough to be free.
    LaunchedEffect(progressKey) {
        while (true) {
            kotlinx.coroutines.delay(10_000)
            if (player.isPlaying) {
                progress.save(progressKey, player.currentPosition, player.duration.coerceAtLeast(0))
            }
        }
    }

    DisposableEffect(Unit) {
        val listener = object : Player.Listener {
            // Clearing the error is the whole point of listening here
            // (SKADI-T-0586). `onPlayerError` only ever SET it, so a momentary
            // network drop left "Couldn't reach the server" on screen forever —
            // including over a film that had recovered and was playing fine.
            //
            // media3 fires this with `null` when the error is cleared, which is
            // exactly the recovery signal the old code had no way to see.
            override fun onPlayerErrorChanged(e: PlaybackException?) {
                if (e == null) error = null
            }

            // Belt and braces: if anything is actually playing, there is no
            // error worth showing, whatever the listener order happened to be.
            override fun onIsPlayingChanged(isPlaying: Boolean) {
                if (isPlaying) error = null
            }

            override fun onPlaybackStateChanged(state: Int) {
                // Finished: forget the position so the next open starts fresh
                // rather than resuming onto the credits — then go to the next
                // episode if there is one. Backing out, finding the show and
                // tapping the next row was the whole reason for the button.
                if (state == Player.STATE_ENDED) {
                    progress.markFinished(progressKey)
                    onNextNow.value?.invoke()
                }
            }

            override fun onPlayerError(e: PlaybackException) {
                // Name the codec problem rather than showing a black screen.
                // `ERROR_CODE_DECODING_FORMAT_UNSUPPORTED` is the DTS case, and
                // an operator who sees it can re-grab in another format instead
                // of assuming the server is broken.
                retryable = when (e.errorCode) {
                    PlaybackException.ERROR_CODE_DECODING_FORMAT_UNSUPPORTED,
                    PlaybackException.ERROR_CODE_DECODER_INIT_FAILED,
                    -> false
                    else -> true
                }
                error = when (e.errorCode) {
                    PlaybackException.ERROR_CODE_DECODING_FORMAT_UNSUPPORTED,
                    PlaybackException.ERROR_CODE_DECODER_INIT_FAILED,
                    ->
                        "This device can't decode this file's audio or video track. " +
                            "Skadi streams the original file without converting it."
                    PlaybackException.ERROR_CODE_IO_BAD_HTTP_STATUS,
                    PlaybackException.ERROR_CODE_IO_NETWORK_CONNECTION_FAILED,
                    ->
                        "Couldn't reach the server for this file."
                    else -> e.errorCodeName
                }
            }
        }
        player.addListener(listener)
        onDispose {
            // The save that matters most: backing out is how people leave a
            // film, and without this the periodic tick above loses up to ten
            // seconds every time.
            val pos = player.currentPosition
            val dur = player.duration.coerceAtLeast(0)
            if (pos > 0) progress.save(progressKey, pos, dur)
            player.removeListener(listener)
            player.release()
        }
    }

    // Landscape + screen-on for the duration. Restored on the way out so the
    // rest of the app is unaffected — a player that leaves the phone pinned
    // landscape after you back out is a bug people remember.
    val activity = context as? Activity
    DisposableEffect(Unit) {
        val previousOrientation = activity?.requestedOrientation
        activity?.requestedOrientation = ActivityInfo.SCREEN_ORIENTATION_SENSOR_LANDSCAPE
        activity?.window?.addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
        onDispose {
            activity?.window?.clearFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
            // Not while the activity is being recreated for a configuration
            // change: restoring portrait from the dying instance while the new
            // one asks for landscape relaunched the activity in a loop, ~3
            // times a second, re-creating the player each time (SKADI-T-0578).
            // The manifest now handles rotation without recreation; this is
            // the second lock on the same door.
            if (activity?.isChangingConfigurations != true) {
                previousOrientation?.let { activity?.requestedOrientation = it }
            }
        }
    }

    Box(modifier = Modifier.fillMaxSize()) {
        AndroidView(
            modifier = Modifier.fillMaxSize(),
            factory = { ctx ->
                PlayerView(ctx).apply {
                    this.player = player
                    useController = true
                    setShowNextButton(false)
                    setShowPreviousButton(false)
                }
            },
        )
        IconButton(
            onClick = onBack,
            modifier = Modifier.align(Alignment.TopStart).padding(8.dp),
        ) {
            Icon(Icons.Filled.ArrowBack, contentDescription = "Back", tint = Color.White)
        }
        if (nextLabel != null && onNext != null) {
            androidx.compose.material3.TextButton(
                onClick = onNext,
                modifier = Modifier.align(Alignment.TopEnd).padding(8.dp),
                colors = androidx.compose.material3.ButtonDefaults.textButtonColors(contentColor = Color.White),
            ) {
                Text("Next · $nextLabel")
                Icon(
                    Icons.Filled.SkipNext,
                    contentDescription = "Play next episode",
                    modifier = Modifier.padding(start = 4.dp),
                )
            }
        }
        error?.let { msg ->
            Box(
                modifier = Modifier.fillMaxSize().padding(32.dp),
                contentAlignment = Alignment.Center,
            ) {
                androidx.compose.foundation.layout.Column(
                    horizontalAlignment = Alignment.CenterHorizontally,
                ) {
                    Text(
                        "$title\n\n$msg",
                        color = MaterialTheme.colorScheme.error,
                        style = MaterialTheme.typography.bodyLarge,
                    )
                    // Clearing the message is not enough on its own
                    // (SKADI-T-0586): after a network error the player has
                    // STOPPED, so it will never recover by itself and the
                    // recovery listener never fires. Something has to call
                    // `prepare()`, and offering that beats making someone back
                    // out and re-enter the film — which also loses their place.
                    if (retryable) {
                        androidx.compose.material3.Button(
                            onClick = {
                                error = null
                                // Resume from where it died rather than the
                                // start; `prepare()` alone would keep the
                                // position, but being explicit survives a
                                // player that reset it.
                                val at = player.currentPosition
                                player.prepare()
                                if (at > 0) player.seekTo(at)
                                player.playWhenReady = true
                            },
                            modifier = Modifier.padding(top = 20.dp),
                        ) {
                            Text("Retry")
                        }
                    }
                }
            }
        }
    }
}
