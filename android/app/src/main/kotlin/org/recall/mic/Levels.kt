package org.recall.mic

import kotlin.math.log10

// Level-meter arithmetic, without Android, so tested on the JVM.

/** The quietest level the meter shows. Far-field speech is around -50 to -40 dBFS. */
const val METER_FLOOR_DBFS = -70f

/**
 * The peak of the first [n] bytes of s16le audio as a 0f..1f meter level, in dBFS
 * above [floorDbfs]: on a linear scale far-field speech would barely move it.
 */
fun peakLevel(buf: ByteArray, n: Int, floorDbfs: Float = METER_FLOOR_DBFS): Float {
    var peak = 0
    var i = 0
    while (i + 1 < n) {
        val sample = (buf[i + 1].toInt() shl 8) or (buf[i].toInt() and 0xFF)
        val amp = if (sample < 0) -sample else sample
        if (amp > peak) peak = amp
        i += 2
    }
    return amplitudeLevel(peak, floorDbfs)
}

/**
 * The meter level for a peak amplitude in 0..32767, such as
 * `MediaRecorder.getMaxAmplitude()` gives the meeting recorder.
 */
fun amplitudeLevel(peak: Int, floorDbfs: Float = METER_FLOOR_DBFS): Float {
    if (peak <= 0) return 0f
    val dbfs = 20f * log10(peak / 32768f)
    return ((dbfs - floorDbfs) / -floorDbfs).coerceIn(0f, 1f)
}

/** Colour tier of one meter segment. */
enum class MeterTier { OFF, LOW, MID, HIGH }

/** Tier of the meter segment at [index], given [lit] of [segments] lit. */
fun meterTier(index: Int, lit: Int, segments: Int): MeterTier =
    when {
        index >= lit -> MeterTier.OFF
        index > segments * 0.85f -> MeterTier.HIGH
        index > segments * 0.6f -> MeterTier.MID
        else -> MeterTier.LOW
    }
