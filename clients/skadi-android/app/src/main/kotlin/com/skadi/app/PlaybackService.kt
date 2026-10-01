package com.skadi.app

import android.content.Intent
import android.os.Handler
import android.os.Looper
import androidx.media3.common.AudioAttributes
import androidx.media3.common.C
import androidx.media3.common.Player
import androidx.media3.exoplayer.ExoPlayer
import androidx.media3.session.MediaSession
import androidx.media3.session.MediaSessionService
import com.skadi.core.OfflineStore
import com.skadi.core.PlayerSettings
import com.skadi.core.SleepTimer

/**
 * Background playback (SKADI-T-0343, hardened in the second review pass):
 * Media3 MediaSessionService owning the ExoPlayer.
 *  - AUDIO FOCUS handled (pauses for calls/other media — ExoPlayer's default
 *    is focus NOT handled; invisible on emulators, glaring on phones);
 *  - WAKE_MODE_LOCAL so screen-off playback survives CPU naps;
 *  - the SLEEP TIMER lives HERE, not in the UI, so it still fires after the
 *    Activity is rotated away or killed (the exact scenario a sleep timer
 *    exists for). [SleepTimer] holds the mode; this service posts a callback
 *    for the exact stop moment and re-posts it on play/pause, seek and speed
 *    change (SKADI-T-0657 — it used to be checked on the 10 s persist tick,
 *    which played up to 10 s into the next chapter). The last 10 s fade out,
 *    a shake in the last minute restarts a minutes timer, and the
 *    notification shows when it will stop (SKADI-T-0658);
 *  - position persisted (atomically, see OfflineStore) on pause + every 10s;
 *    finished = >= 98% listened, feeding next-in-series.
 */
class PlaybackService : MediaSessionService() {
    private var session: MediaSession? = null
    private lateinit var store: OfflineStore
    private val handler = Handler(Looper.getMainLooper())

    private val persist = object : Runnable {
        override fun run() {
            savePosition()
            handler.postDelayed(this, 10_000)
        }
    }

    /**
     * The chapter end an armed End of chapter is counting down to. Fixed when
     * scheduled (play, seek, speed change), so a tick that lands a few ms into
     * the next chapter still knows which end it was waiting for.
     */
    private var scheduledEndS: Double? = null

    private val sleepTick = Runnable { onSleepTick() }
    private lateinit var settings: PlayerSettings
    private var shake: ShakeDetector? = null

    override fun onCreate() {
        super.onCreate()
        store = OfflineStore(filesDir)
        settings = PlayerSettings(this)
        val player = ExoPlayer.Builder(this)
            .setAudioAttributes(
                AudioAttributes.Builder()
                    .setUsage(C.USAGE_MEDIA)
                    .setContentType(C.AUDIO_CONTENT_TYPE_SPEECH)
                    .build(),
                /* handleAudioFocus = */ true,
            )
            .setWakeMode(C.WAKE_MODE_LOCAL)
            .setHandleAudioBecomingNoisy(true)
            .build()
        player.addListener(object : Player.Listener {
            override fun onIsPlayingChanged(isPlaying: Boolean) {
                sleep.onPlayingChanged(isPlaying)
                scheduleSleep()
                if (isPlaying) {
                    handler.removeCallbacks(persist)
                    handler.post(persist)
                } else {
                    handler.removeCallbacks(persist)
                    savePosition()
                }
            }

            override fun onPositionDiscontinuity(
                oldPosition: Player.PositionInfo,
                newPosition: Player.PositionInfo,
                reason: Int,
            ) = scheduleSleep()

            override fun onPlaybackParametersChanged(
                playbackParameters: androidx.media3.common.PlaybackParameters,
            ) = scheduleSleep()
        })
        session = MediaSession.Builder(this, player).build()
        instance = this
    }

    private fun savePosition() {
        val p = session?.player ?: return
        val fid = p.currentMediaItem?.mediaId ?: return
        val durMs = p.duration.takeIf { it > 0 } ?: return
        val posMs = p.currentPosition
        val finished = posMs.toDouble() / durMs >= 0.98
        store.updateProgress(fid, posMs / 1000.0, finished)
    }

    /**
     * Re-plan the sleep timer from the player's state now. Called on arm,
     * cancel, play/pause, seek and speed change. Nothing runs while paused: a
     * minutes timer is stopped, and the chapter end does not come closer.
     */
    private fun scheduleSleep() {
        handler.removeCallbacks(sleepTick)
        scheduledEndS = null
        val p = session?.player ?: return
        if (sleep.mode == SleepTimer.Mode.EndOfChapter) {
            scheduledEndS = SleepTimer.chapterEndS(chaptersOf(p), p.currentPosition / 1000.0)
        }
        showSleepInNotification(p)
        if (!p.isPlaying || sleep.mode == null) {
            endFade(p)
            return
        }
        onSleepTick()
    }

    /** Wall-clock ms until the stop, or null when nothing will stop playback. */
    private fun msUntilStop(p: Player): Long? = when (sleep.mode) {
        null -> null
        is SleepTimer.Mode.Minutes -> sleep.remainingMs()
        SleepTimer.Mode.EndOfChapter -> scheduledEndS?.let { end ->
            val left = (end - p.currentPosition / 1000.0) / p.playbackParameters.speed.coerceAtLeast(0.1f)
            // A tick that lands a hair past the end, or within the 0.3 s it
            // takes to act, is due now.
            if (left <= 0.3) 0L else (left * 1000).toLong()
        }
    }

    private fun onSleepTick() {
        val p = session?.player ?: return
        val ms = msUntilStop(p)
        if (ms == null) {
            endFade(p)
            return
        }
        if (ms <= 50L) {
            sleep.cancel()
            scheduledEndS = null
            p.pause()
            endFade(p)
            showSleepInNotification(p)
            return
        }
        p.volume = SleepTimer.fadeVolume(ms)
        val wantShake = sleep.mode is SleepTimer.Mode.Minutes &&
            ms <= SleepTimer.SHAKE_WINDOW_MS && settings.shakeToExtend
        if (wantShake && shake == null) {
            shake = ShakeDetector(this) {
                sleep.restart(playing = p.isPlaying)
                scheduleSleep()
            }.also { it.start() }
        } else if (!wantShake) {
            stopShake()
        }
        handler.postDelayed(sleepTick, SleepTimer.nextTickMs(ms))
    }

    /** Full volume and no shake listener: the state outside a fade. */
    private fun endFade(p: Player) {
        p.volume = 1f
        stopShake()
    }

    private fun stopShake() {
        shake?.stop()
        shake = null
    }

    /**
     * Put the sleep timer in the notification's second line, after the
     * author: the time it will stop ("Sleep at 11:41 PM"), which changes only
     * when the plan does, rather than a countdown the notification would have
     * to redraw every second. Updated in place with [Player.replaceMediaItem]:
     * same URI, so ExoPlayer updates the metadata without re-preparing.
     */
    private fun showSleepInNotification(p: Player) {
        val item = p.currentMediaItem ?: return
        val md = item.mediaMetadata
        val base = md.extras?.getString(BASE_ARTIST) ?: md.artist?.toString().orEmpty()
        val status = when (sleep.mode) {
            null -> null
            is SleepTimer.Mode.Minutes -> if (p.isPlaying) {
                val at = System.currentTimeMillis() + (sleep.remainingMs() ?: 0L)
                "Sleep at " + android.text.format.DateFormat.getTimeFormat(this).format(java.util.Date(at))
            } else {
                "Sleep in " + SleepTimer.clock(sleep.remainingMs() ?: 0L)
            }
            SleepTimer.Mode.EndOfChapter -> "Sleep at chapter end"
        }
        val artist = listOfNotNull(base.ifEmpty { null }, status).joinToString(" · ")
        if (artist == md.artist?.toString().orEmpty()) return
        val extras = android.os.Bundle().apply { putString(BASE_ARTIST, base) }
        val updated = item.buildUpon()
            .setMediaMetadata(md.buildUpon().setArtist(artist).setExtras(extras).build())
            .build()
        p.replaceMediaItem(p.currentMediaItemIndex, updated)
    }

    private fun chaptersOf(p: Player): List<com.skadi.core.Chapter> {
        val fid = p.currentMediaItem?.mediaId ?: return emptyList()
        if (fid != chaptersFid) {
            chaptersFid = fid
            chaptersCache = store.chapters(fid)
        }
        return chaptersCache
    }

    private var chaptersFid: String? = null
    private var chaptersCache: List<com.skadi.core.Chapter> = emptyList()

    override fun onGetSession(controllerInfo: MediaSession.ControllerInfo): MediaSession? = session

    override fun onTaskRemoved(rootIntent: Intent?) {
        val p = session?.player
        if (p == null || !p.playWhenReady) stopSelf()
    }

    override fun onDestroy() {
        if (instance === this) instance = null
        handler.removeCallbacks(persist)
        handler.removeCallbacks(sleepTick)
        stopShake()
        savePosition()
        session?.run {
            player.release()
            release()
        }
        session = null
        super.onDestroy()
    }

    companion object {
        /**
         * The sleep timer. Same-process state shared with the UI, which reads
         * it for the label and changes it only through [armSleepMinutes],
         * [armSleepEndOfChapter] and [cancelSleep], so the service reschedules.
         */
        val sleep = SleepTimer()

        private var instance: PlaybackService? = null

        /** The author line before the sleep status was appended to it. */
        private const val BASE_ARTIST = "skadi.base_artist"

        private fun playing() = instance?.session?.player?.isPlaying == true

        fun armSleepMinutes(minutes: Int) {
            sleep.armMinutes(minutes, playing())
            instance?.scheduleSleep()
        }

        fun armSleepEndOfChapter() {
            sleep.armEndOfChapter()
            instance?.scheduleSleep()
        }

        fun cancelSleep() {
            sleep.cancel()
            instance?.scheduleSleep()
        }
    }
}
