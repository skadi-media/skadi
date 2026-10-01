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
import androidx.compose.material.icons.filled.Subtitles
import androidx.compose.foundation.verticalScroll
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
    val trackPrefs = remember { com.skadi.core.TrackPreferences(context) }
    var showLanguages by remember { mutableStateOf(false) }
    // Set while a track change is the viewer's, made in the player's own menu,
    // rather than ours: only theirs is remembered for the series.
    val viewerChose = remember { java.util.concurrent.atomic.AtomicBoolean(false) }

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
            .build()
    }

    // Load once the video's subtitle files are known (SKADI-T-0663): they
    // must be part of the MediaItem, and appear in the player's own track
    // menu beside any embedded ones. One small request; with none, or an
    // older server, the film plays without them.
    LaunchedEffect(url) {
        // Start with the viewer's languages (SKADI-T-0664) — the series' own
        // choice when this is an episode, else the global one.
        applyTrackPrefs(player, trackPrefs.effective(progressKey))
        val subs = com.skadi.core.Subtitles.fetch(url)
        player.setMediaItem(
            MediaItem.Builder()
                .setUri(url)
                .setSubtitleConfigurations(subs.map { t -> subtitleConfiguration(url, t) })
                .build(),
        )
        // Seek BEFORE prepare: media3 applies a pending seek as the start
        // position, so the film opens where you left it instead of playing a
        // second from the top and jumping.
        resumeAt?.let { player.seekTo(it) }
        player.prepare()
        player.playWhenReady = true
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

            // A track picked in the player's menu, during an episode, is that
            // series' choice from now on (SKADI-T-0664). Our own changes go
            // through [applyTrackPrefs] with the flag clear.
            override fun onTrackSelectionParametersChanged(
                parameters: androidx.media3.common.TrackSelectionParameters,
            ) {
                if (!applyingPrefs) viewerChose.set(true)
            }

            override fun onTracksChanged(tracks: androidx.media3.common.Tracks) {
                if (!viewerChose.getAndSet(false)) return
                val seriesId = com.skadi.core.TrackPreferences.seriesIdOf(progressKey) ?: return
                trackPrefs.setSeries(seriesId, chosenPrefs(tracks))
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
        IconButton(
            onClick = { showLanguages = true },
            modifier = Modifier.align(Alignment.TopStart).padding(start = 56.dp, top = 8.dp),
        ) {
            Icon(
                androidx.compose.material.icons.Icons.Filled.Subtitles,
                contentDescription = "Languages",
                tint = Color.White,
            )
        }
        if (showLanguages) {
            LanguagesDialog(
                current = trackPrefs.global,
                onDismiss = { showLanguages = false },
                onSave = { p ->
                    trackPrefs.global = p
                    com.skadi.core.TrackPreferences.seriesIdOf(progressKey)
                        ?.let { trackPrefs.setSeries(it, p) }
                    applyTrackPrefs(player, p)
                    showLanguages = false
                },
            )
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

/** One server subtitle file as a media3 side-loaded track (SKADI-T-0663). */
private fun subtitleConfiguration(
    videoUrl: String,
    t: com.skadi.core.SubtitleTrack,
): MediaItem.SubtitleConfiguration =
    MediaItem.SubtitleConfiguration.Builder(
        android.net.Uri.parse(com.skadi.core.Subtitles.fileUrl(videoUrl, t.index)),
    )
        .setMimeType(
            when (t.format) {
                "vtt" -> androidx.media3.common.MimeTypes.TEXT_VTT
                "ass", "ssa" -> androidx.media3.common.MimeTypes.TEXT_SSA
                else -> androidx.media3.common.MimeTypes.APPLICATION_SUBRIP
            },
        )
        .setLanguage(t.language)
        .setLabel(t.label)
        .setSelectionFlags(if (t.forced) androidx.media3.common.C.SELECTION_FLAG_FORCED else 0)
        .setId("skadi-sub-${t.index}")
        .build()

/** True while [applyTrackPrefs] is changing the selection, so it is not taken for the viewer's choice. */
@Volatile
private var applyingPrefs = false

/** Point the player's track selection at [p] (SKADI-T-0664). */
private fun applyTrackPrefs(player: Player, p: com.skadi.core.TrackPrefs) {
    val text = androidx.media3.common.C.TRACK_TYPE_TEXT
    val b = player.trackSelectionParameters.buildUpon()
        .clearOverridesOfType(androidx.media3.common.C.TRACK_TYPE_AUDIO)
        .clearOverridesOfType(text)
        .setPreferredAudioLanguage(p.audio)
    when (p.subtitles) {
        com.skadi.core.SubtitleMode.Off -> b.setTrackTypeDisabled(text, true)
        // No preferred text language, and "default"-flagged subtitles ignored:
        // only forced ones are picked.
        com.skadi.core.SubtitleMode.Forced -> b.setTrackTypeDisabled(text, false)
            .setPreferredTextLanguage(null)
            .setIgnoredTextSelectionFlags(androidx.media3.common.C.SELECTION_FLAG_DEFAULT)
        com.skadi.core.SubtitleMode.On -> b.setTrackTypeDisabled(text, false)
            .setPreferredTextLanguage(p.subtitleLanguage ?: p.audio)
            .setIgnoredTextSelectionFlags(0)
            .setSelectUndeterminedTextLanguage(true)
    }
    applyingPrefs = true
    try {
        player.trackSelectionParameters = b.build()
    } finally {
        applyingPrefs = false
    }
}

/** The preferences that reproduce what is selected now. */
private fun chosenPrefs(tracks: androidx.media3.common.Tracks): com.skadi.core.TrackPrefs {
    fun selected(type: Int) = tracks.groups
        .filter { it.type == type && it.isSelected }
        .firstNotNullOfOrNull { g -> (0 until g.length).firstOrNull { g.isTrackSelected(it) }?.let { g.getTrackFormat(it) } }
    val audio = selected(androidx.media3.common.C.TRACK_TYPE_AUDIO)
    val text = selected(androidx.media3.common.C.TRACK_TYPE_TEXT)
    return com.skadi.core.TrackPrefs(
        audio = audio?.language,
        subtitles = when {
            text == null -> com.skadi.core.SubtitleMode.Off
            text.selectionFlags and androidx.media3.common.C.SELECTION_FLAG_FORCED != 0 ->
                com.skadi.core.SubtitleMode.Forced
            else -> com.skadi.core.SubtitleMode.On
        },
        subtitleLanguage = text?.language,
    )
}

/** The viewer's languages, for every film and episode (SKADI-T-0664). */
@Composable
private fun LanguagesDialog(
    current: com.skadi.core.TrackPrefs,
    onDismiss: () -> Unit,
    onSave: (com.skadi.core.TrackPrefs) -> Unit,
) {
    var audio by remember { mutableStateOf(current.audio) }
    var mode by remember { mutableStateOf(current.subtitles) }
    var subLang by remember { mutableStateOf(current.subtitleLanguage) }
    val langs = com.skadi.core.TrackPreferences.LANGUAGES
    androidx.compose.material3.AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text("Languages") },
        text = {
            androidx.compose.foundation.layout.Column(
                modifier = Modifier.verticalScroll(androidx.compose.foundation.rememberScrollState()),
            ) {
                Text("Audio", style = androidx.compose.material3.MaterialTheme.typography.titleSmall)
                ChipRow(listOf(null to "File default") + langs, audio) { audio = it }
                Text(
                    "Subtitles",
                    style = androidx.compose.material3.MaterialTheme.typography.titleSmall,
                    modifier = Modifier.padding(top = 12.dp),
                )
                ChipRow(
                    listOf(
                        com.skadi.core.SubtitleMode.Off to "Off",
                        com.skadi.core.SubtitleMode.Forced to "Forced only",
                        com.skadi.core.SubtitleMode.On to "On",
                    ),
                    mode,
                ) { mode = it }
                if (mode == com.skadi.core.SubtitleMode.On) {
                    Text(
                        "Subtitle language",
                        style = androidx.compose.material3.MaterialTheme.typography.titleSmall,
                        modifier = Modifier.padding(top = 12.dp),
                    )
                    ChipRow(listOf(null to "Same as audio") + langs, subLang) { subLang = it }
                }
                Text(
                    "Used for every film and episode. A track you pick in the player during a series is remembered for that series.",
                    style = androidx.compose.material3.MaterialTheme.typography.bodySmall,
                    modifier = Modifier.padding(top = 12.dp),
                )
            }
        },
        confirmButton = {
            androidx.compose.material3.TextButton(onClick = {
                onSave(com.skadi.core.TrackPrefs(audio, mode, subLang))
            }) { Text("Save") }
        },
        dismissButton = {
            androidx.compose.material3.TextButton(onClick = onDismiss) { Text("Cancel") }
        },
    )
}

@OptIn(androidx.compose.foundation.layout.ExperimentalLayoutApi::class)
@Composable
private fun <T> ChipRow(options: List<Pair<T, String>>, selected: T, onPick: (T) -> Unit) {
    androidx.compose.foundation.layout.FlowRow(
        horizontalArrangement = androidx.compose.foundation.layout.Arrangement.spacedBy(6.dp),
        modifier = Modifier.padding(top = 6.dp),
    ) {
        options.forEach { (value, label) ->
            androidx.compose.material3.FilterChip(
                selected = value == selected,
                onClick = { onPick(value) },
                label = { Text(label) },
            )
        }
    }
}
