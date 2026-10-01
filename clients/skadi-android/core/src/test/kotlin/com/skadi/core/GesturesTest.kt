package com.skadi.core

import org.junit.Assert.assertEquals
import org.junit.Test

/** SKADI-T-0667. */
class GesturesTest {
    @Test
    fun `thirds of the screen`() {
        assertEquals(Gestures.Zone.Left, Gestures.zone(100f, 900f))
        assertEquals(Gestures.Zone.Middle, Gestures.zone(450f, 900f))
        assertEquals(Gestures.Zone.Right, Gestures.zone(800f, 900f))
    }

    @Test
    fun `swiping up raises the level, a full height covers the range`() {
        assertEquals(0.5f, Gestures.levelDelta(-500f, 1000f), 0.0001f)
        assertEquals(-1f, Gestures.levelDelta(1000f, 1000f), 0.0001f)
        assertEquals(1f, Gestures.clampLevel(1.4f), 0f)
        assertEquals(0f, Gestures.clampLevel(-0.2f), 0f)
    }

    @Test
    fun `levels become whole volume steps`() {
        assertEquals(8, Gestures.toSteps(0.5f, 15))
        assertEquals(15, Gestures.toSteps(1.2f, 15))
        assertEquals(0, Gestures.toSteps(0f, 15))
    }
}
