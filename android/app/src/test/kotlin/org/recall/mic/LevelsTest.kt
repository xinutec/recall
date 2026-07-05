package org.recall.mic

import org.junit.Assert.assertEquals
import org.junit.Test

class LevelsTest {
    @Test
    fun silenceReadsZero() {
        assertEquals(0f, peakLevel(ByteArray(64), 64))
    }

    @Test
    fun fullScaleReadsOne() {
        // +32767 as little-endian s16le is 0xFF 0x7F
        assertEquals(1f, peakLevel(byteArrayOf(0xFF.toByte(), 0x7F), 2), 1e-3f)
    }

    @Test
    fun belowFloorReadsZero() {
        // amplitude 1 is about -90 dBFS, below the -70 floor
        assertEquals(0f, peakLevel(byteArrayOf(0x01, 0x00), 2))
    }

    @Test
    fun farFieldSpeechLandsMidMeter() {
        // a ~-44 dBFS peak (amp ~207) should read mid-range, not ~0 like a linear meter
        val amp = 207
        val buf = byteArrayOf((amp and 0xFF).toByte(), (amp shr 8).toByte())
        val level = peakLevel(buf, 2)
        assert(level in 0.25f..0.45f) { "expected mid-range, got $level" }
    }

    @Test
    fun onlyTheFirstNBytesAreScanned() {
        // the loud sample past n must be ignored
        assertEquals(0f, peakLevel(byteArrayOf(0x00, 0x00, 0xFF.toByte(), 0x7F), 2))
    }

    @Test
    fun meterTiersClassifyByPosition() {
        assertEquals(MeterTier.OFF, meterTier(index = 10, lit = 5, segments = 24))
        assertEquals(MeterTier.LOW, meterTier(index = 0, lit = 24, segments = 24))
        assertEquals(MeterTier.MID, meterTier(index = 16, lit = 24, segments = 24))
        assertEquals(MeterTier.HIGH, meterTier(index = 23, lit = 24, segments = 24))
    }
}
