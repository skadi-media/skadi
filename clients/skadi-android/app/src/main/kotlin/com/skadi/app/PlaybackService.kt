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
 *    which played up to 10 s into the next chapter);
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

    /** End-of-chapter target the pending [sleepCheck] was scheduled for. */
    private var scheduledEndS: Double? = null

    private val sleepCheck = Runnable { onSleepDue() }

    override fun onCreate() {
        super.onCreate()
        store = OfflineStore(filesDir)
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
     * Post [sleepCheck] for the moment playback must stop. Nothing is posted
     * while paused: a minutes timer does not run, and the chapter end does not
     * come closer.
     */
    private fun scheduleSleep() {
        handler.removeCallbacks(sleepCheck)
        scheduledEndS = null
        val p = session?.player ?: return
        if (!p.isPlaying) return
        val chapters = chaptersOf(p)
        val posS = p.currentPosition / 1000.0
        val ms = sleep.msUntilStop(posS, p.playbackParameters.speed, chapters) ?: return
        if (sleep.mode == SleepTimer.Mode.EndOfChapter) {
            scheduledEndS = SleepTimer.chapterEndS(chapters, posS)
        }
        handler.postDelayed(sleepCheck, ms)
    }

    private fun onSleepDue() {
        val p = session?.player ?: return
        val due = when (sleep.mode) {
            null -> false
            is SleepTimer.Mode.Minutes -> (sleep.remainingMs() ?: 0L) <= 50L
            // Judged against the end this check was scheduled for: a callback
            // that runs a few ms late is already in the next chapter, whose own
            // end is far away.
            SleepTimer.Mode.EndOfChapter ->
                scheduledEndS?.let { p.currentPosition / 1000.0 >= it - 0.3 } ?: false
        }
        if (due) {
            sleep.cancel()
            scheduledEndS = null
            p.pause()
        } else {
            scheduleSleep()
        }
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
        handler.removeCallbacks(sleepCheck)
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
