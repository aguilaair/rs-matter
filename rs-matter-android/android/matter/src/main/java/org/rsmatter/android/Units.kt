package org.rsmatter.android

import kotlin.math.log10
import kotlin.math.roundToInt

/** Readings in the units Matter's measurement clusters use. Pure, for tests. */
object Units {
    /** `IlluminanceMeasurement.MeasuredValue`: `10000 * log10(lux) + 1`, 0 for darkness, at most 0xFFFE. */
    fun illuminance(lux: Double): Int = when {
        lux.isNaN() || lux <= 0.0 -> 0
        else -> (10000 * log10(lux) + 1).roundToInt().coerceIn(1, 0xFFFE)
    }

    /** Temperature (0.01 °C) and relative humidity (0.01 %). */
    fun hundredths(v: Double): Int = if (v.isNaN()) Int.MIN_VALUE else (v * 100).roundToInt()
}
