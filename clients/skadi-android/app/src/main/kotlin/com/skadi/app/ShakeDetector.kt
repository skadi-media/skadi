package com.skadi.app

import android.content.Context
import android.hardware.Sensor
import android.hardware.SensorEvent
import android.hardware.SensorEventListener
import android.hardware.SensorManager
import kotlin.math.sqrt

/**
 * Calls [onShake] when the phone is shaken (SKADI-T-0658: shake to extend the
 * sleep timer). Listens only between [start] and [stop]; the service starts it
 * in a sleep timer's last minute, so it costs nothing the rest of the time.
 *
 * A shake is two jolts over [THRESHOLD_G] within [WINDOW_MS] — one jolt is
 * a phone put down on a nightstand. After a shake it stays quiet for
 * [COOLDOWN_MS], so one vigorous shake is one extension.
 */
class ShakeDetector(context: Context, private val onShake: () -> Unit) : SensorEventListener {
    private val sensors = context.getSystemService(Context.SENSOR_SERVICE) as SensorManager
    private var firstJoltAt = 0L
    private var lastShakeAt = 0L

    fun start() {
        sensors.getDefaultSensor(Sensor.TYPE_ACCELEROMETER)?.let {
            sensors.registerListener(this, it, SensorManager.SENSOR_DELAY_UI)
        }
    }

    fun stop() = sensors.unregisterListener(this)

    override fun onSensorChanged(event: SensorEvent) {
        val (x, y, z) = event.values
        val g = sqrt(x * x + y * y + z * z) / SensorManager.GRAVITY_EARTH
        if (g < THRESHOLD_G) return
        val now = System.currentTimeMillis()
        if (now - lastShakeAt < COOLDOWN_MS) return
        if (now - firstJoltAt > WINDOW_MS) {
            firstJoltAt = now
            return
        }
        // Samples above the threshold for the next few ms are the same jolt.
        if (now - firstJoltAt < SAME_JOLT_MS) return
        firstJoltAt = 0L
        lastShakeAt = now
        onShake()
    }

    override fun onAccuracyChanged(sensor: Sensor?, accuracy: Int) = Unit

    private companion object {
        const val THRESHOLD_G = 2.2f
        const val WINDOW_MS = 600L
        const val SAME_JOLT_MS = 120L
        const val COOLDOWN_MS = 2_000L
    }
}
