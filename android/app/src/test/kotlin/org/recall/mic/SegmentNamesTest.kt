package org.recall.mic

import org.junit.Assert.assertEquals
import org.junit.Test
import java.time.Instant

class SegmentNamesTest {
    @Test
    fun `a segment is named by its source and UTC open instant`() {
        val name = SegmentNames.segmentName("pixel5", Instant.parse("2026-09-05T12:00:00Z"))
        assertEquals("pixel5-20260905T120000.wav", name)
    }

    @Test
    fun `the stamp is UTC whatever the device zone thinks`() {
        // 23:30Z is the next local day in most of Europe; the name must not care.
        val name = SegmentNames.segmentName("usb", Instant.parse("2026-12-31T23:30:00Z"))
        assertEquals("usb-20261231T233000.wav", name)
    }

    @Test
    fun `one minute of PCM is the rotation boundary`() {
        assertEquals(60 * 48_000 * 2, SegmentNames.SEGMENT_BYTES)
    }

    @Test
    fun `the wav header states the audio truthfully`() {
        val h = SegmentNames.wavHeader(96_000) // one second of 48k mono s16
        assertEquals("RIFF", String(h, 0, 4))
        assertEquals("WAVE", String(h, 8, 4))
        assertEquals("fmt ", String(h, 12, 4))
        assertEquals("data", String(h, 36, 4))

        fun le32(o: Int) =
            (h[o].toInt() and 0xFF) or ((h[o + 1].toInt() and 0xFF) shl 8) or
                ((h[o + 2].toInt() and 0xFF) shl 16) or ((h[o + 3].toInt() and 0xFF) shl 24)

        fun le16(o: Int) = (h[o].toInt() and 0xFF) or ((h[o + 1].toInt() and 0xFF) shl 8)
        assertEquals(36 + 96_000, le32(4))
        assertEquals(1, le16(20)) // PCM
        assertEquals(1, le16(22)) // mono
        assertEquals(48_000, le32(24))
        assertEquals(96_000, le32(28)) // byte rate
        assertEquals(16, le16(34)) // bit depth
        assertEquals(96_000, le32(40)) // data size
    }
}
