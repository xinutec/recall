package org.recall.mic

import kotlin.math.log10

// Pure audio-level and meter logic — no Android or Compose dependencies, so it's
// unit-tested on the JVM (the service and UI just call into it).

/** Quietest level the meter shows; below this (room noise) reads empty. Far-field
 * speech sits around -50..-40 dBFS, so this gives it real range. */
const val METER_FLOOR_DBFS = -70f

/**
 * Peak amplitude of the first [n] bytes of an s16le buffer as a 0f..1f meter
 * level. Scaled logarithmically (dBFS) from [floorDbfs], because far-field speech
 * is tiny in linear terms (a loud distant voice is only ~-44 dBFS) and a linear
 * meter barely moves.
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
 * The same 0f..1f meter scaling for a peak amplitude that was measured elsewhere —
 * `MediaRecorder.getMaxAmplitude()` reports the same 0..32767 units but never hands over
 * the samples, so the meeting recorder can't compute it from a buffer the way
 * [peakLevel] does. One function so both modes' meters read alike.
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
