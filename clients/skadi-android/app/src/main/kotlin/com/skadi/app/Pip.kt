package com.skadi.app

import android.app.Activity
import android.app.PendingIntent
import android.app.PictureInPictureParams
import android.app.RemoteAction
import android.content.Context
import android.content.Intent
import android.graphics.drawable.Icon
import android.os.Build
import android.util.Rational
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue

/**
 * Picture-in-picture for the video player (SKADI-T-0665).
 *
 * The activity owns the PiP window; the player owns playback. This object is
 * the hand-off: the player says when a video is on screen and what its PiP
 * actions do, and the activity enters PiP when the viewer leaves while one is.
 * The audiobook player never registers, so leaving it behaves as before.
 */
object Pip {
    /** Whether the activity is shown as a PiP window now; the player hides its overlays. */
    var inPip by mutableStateOf(false)
        internal set

    /** The video on screen, or null. */
    internal var video: Video? = null

    class Video(
        val isPlaying: () -> Boolean,
        val togglePlay: () -> Unit,
        val seekBy: (Long) -> Unit,
        /** The PiP window was closed: stop and save. */
        val onDismissed: () -> Unit,
        val aspect: () -> Rational?,
    )

    const val ACTION = "com.skadi.app.PIP"
    const val EXTRA = "op"
    const val OP_TOGGLE = 1
    const val OP_BACK = 2
    const val OP_FORWARD = 3

    fun params(context: Context): PictureInPictureParams {
        val v = video
        val b = PictureInPictureParams.Builder()
        v?.aspect()?.let { r ->
            // Android rejects ratios outside 1:2.39 .. 2.39:1.
            val x = r.toFloat().coerceIn(1 / 2.39f, 2.39f)
            b.setAspectRatio(Rational((x * 1000).toInt(), 1000))
        }
        if (v != null) {
            b.setActions(
                listOf(
                    action(context, OP_BACK, android.R.drawable.ic_media_rew, "Back 10 seconds"),
                    if (v.isPlaying()) {
                        action(context, OP_TOGGLE, android.R.drawable.ic_media_pause, "Pause")
                    } else {
                        action(context, OP_TOGGLE, android.R.drawable.ic_media_play, "Play")
                    },
                    action(context, OP_FORWARD, android.R.drawable.ic_media_ff, "Forward 10 seconds"),
                ),
            )
        }
        // Android 12+: go to PiP on the home gesture without waiting for
        // onUserLeaveHint, and only while a video is on screen.
        if (Build.VERSION.SDK_INT >= 31) b.setAutoEnterEnabled(v != null)
        return b.build()
    }

    private fun action(context: Context, op: Int, icon: Int, title: String): RemoteAction {
        val intent = Intent(ACTION).setPackage(context.packageName).putExtra(EXTRA, op)
        val pi = PendingIntent.getBroadcast(
            context,
            op,
            intent,
            PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
        )
        return RemoteAction(Icon.createWithResource(context, icon), title, title, pi)
    }

    /** A PiP action was tapped. */
    fun handle(op: Int) {
        val v = video ?: return
        when (op) {
            OP_TOGGLE -> v.togglePlay()
            OP_BACK -> v.seekBy(-10_000)
            OP_FORWARD -> v.seekBy(10_000)
        }
    }

    /** Refresh the window's actions, e.g. after play/pause. */
    fun update(activity: Activity?) {
        activity ?: return
        runCatching { activity.setPictureInPictureParams(params(activity)) }
    }
}
