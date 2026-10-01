package com.skadi.app

import androidx.media3.common.ForwardingPlayer
import androidx.media3.common.Player
import com.skadi.core.PlayerSettings

/**
 * The player the media session exposes (SKADI-T-0661): skip lengths come from
 * the listener's settings, read at each skip.
 *
 * ExoPlayer fixes its seek increments when it is built, so changing them in
 * settings would need a new player. Wrapping it means the notification, the
 * headset, a car and the app's own buttons all skip by the same, current
 * length.
 *
 * An audiobook is one long item, so "next" and "previous" — the headset's
 * double and triple press, and the notification's outer buttons — skip
 * forward and back instead of jumping to the end or the start of the book.
 */
class SkipPlayer(player: Player, private val settings: PlayerSettings) : ForwardingPlayer(player) {
    override fun getSeekBackIncrement(): Long = settings.skipBackS * 1000L
    override fun getSeekForwardIncrement(): Long = settings.skipForwardS * 1000L

    override fun seekBack() = seekTo((currentPosition - seekBackIncrement).coerceAtLeast(0L))

    override fun seekForward() {
        val target = currentPosition + seekForwardIncrement
        val end = duration
        seekTo(if (end > 0) minOf(target, end) else target)
    }

    override fun seekToNext() = seekForward()
    override fun seekToPrevious() = seekBack()
    override fun seekToNextMediaItem() = seekForward()
    override fun seekToPreviousMediaItem() = seekBack()

    override fun isCommandAvailable(command: Int): Boolean =
        command in SKIP_COMMANDS || super.isCommandAvailable(command)

    override fun getAvailableCommands(): Player.Commands =
        super.getAvailableCommands().buildUpon().addAll(*SKIP_COMMANDS.toIntArray()).build()

    private companion object {
        val SKIP_COMMANDS = listOf(
            Player.COMMAND_SEEK_BACK,
            Player.COMMAND_SEEK_FORWARD,
            Player.COMMAND_SEEK_TO_NEXT,
            Player.COMMAND_SEEK_TO_PREVIOUS,
            Player.COMMAND_SEEK_TO_NEXT_MEDIA_ITEM,
            Player.COMMAND_SEEK_TO_PREVIOUS_MEDIA_ITEM,
        )
    }
}
