package com.skadi.core

/**
 * Video player touch gestures (SKADI-T-0667): the arithmetic, kept apart from
 * Compose so it can be tested.
 */
object Gestures {
    enum class Zone { Left, Middle, Right }

    /** Which third of the screen [x] falls in. */
    fun zone(x: Float, width: Float): Zone = when {
        width <= 0f -> Zone.Middle
        x < width / 3f -> Zone.Left
        x > width * 2f / 3f -> Zone.Right
        else -> Zone.Middle
    }

    /** One double-tap's seek, in ms. */
    const val SEEK_STEP_MS = 10_000L

    /**
     * A vertical drag of [dy] px (negative = upward) on a screen [height] px
     * tall changes a 0..1 level by this much: a full-height swipe covers the
     * whole range, and up means more.
     */
    fun levelDelta(dy: Float, height: Float): Float = if (height <= 0f) 0f else -dy / height

    fun clampLevel(level: Float): Float = level.coerceIn(0f, 1f)

    /** A 0..1 level as a whole step of [max] (volume has 15-ish steps). */
    fun toSteps(level: Float, max: Int): Int = Math.round(clampLevel(level) * max)
}
