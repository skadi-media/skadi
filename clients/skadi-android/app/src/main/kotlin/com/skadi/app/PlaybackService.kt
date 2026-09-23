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
import java.util.concurrent.atomic.AtomicLong

/**
 * Background playback (SKADI-T-0343, hardened in the second review pass):
 * Media3 MediaSessionService owning the ExoPlayer.
 *  - AUDIO FOCUS handled (pauses for calls/other media — ExoPlayer's default
 *    is focus NOT handled; invisible on emulators, glaring on phones);
 *  - WAKE_MODE_LOCAL so screen-off playback survives CPU naps;
 *  - the SLEEP TIMER lives HERE, not in the UI: a deadline the persist tick
 *    enforces, so it still fires after the Activity is rotated away or killed
 *    (the exact scenario a sleep timer exists for);
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
            enforceSleep()
            handler.postDelayed(this, 10_000)
        }
    }

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
                if (isPlaying) {
                    handler.removeCallbacks(persist)
                    handler.post(persist)
                } else {
                    handler.removeCallbacks(persist)
                    savePosition()
                }
            }
        })
        session = MediaSession.Builder(this, player).build()
    }

    private fun savePosition() {
        val p = session?.player ?: return
        val fid = p.currentMediaItem?.mediaId ?: return
        val durMs = p.duration.takeIf { it > 0 } ?: return
        val posMs = p.currentPosition
        val finished = posMs.toDouble() / durMs >= 0.98
        store.updateProgress(fid, posMs / 1000.0, finished)
    }

    private fun enforceSleep() {
        val p = session?.player ?: return
        val deadline = sleepAtMs.get()
        if (deadline > 0 && System.currentTimeMillis() >= deadline) {
            sleepAtMs.set(0)
            p.pause()
        }
        val eocEnd = sleepEocGet()
        if (eocEnd > 0 && p.currentPosition / 1000.0 >= eocEnd - 0.5) {
            sleepEocSet(0.0)
            p.pause()
        }
    }

    override fun onGetSession(controllerInfo: MediaSession.ControllerInfo): MediaSession? = session

    override fun onTaskRemoved(rootIntent: Intent?) {
        val p = session?.player
        if (p == null || !p.playWhenReady) stopSelf()
    }

    override fun onDestroy() {
        handler.removeCallbacks(persist)
        savePosition()
        session?.run {
            player.release()
            release()
        }
        session = null
        super.onDestroy()
    }

    companion object {
        /** Wall-clock sleep deadline (epoch ms); 0 = off. Same-process state
         *  shared with the UI — enforced here so it survives the Activity. */
        val sleepAtMs = AtomicLong(0)

        /** End-of-chapter sleep target (audio seconds) as Double bits; 0 = off. */
        private val sleepEocBits = AtomicLong(0)
        fun sleepEocGet(): Double = Double.fromBits(sleepEocBits.get())
        fun sleepEocSet(v: Double) = sleepEocBits.set(v.toRawBits())
    }
}
