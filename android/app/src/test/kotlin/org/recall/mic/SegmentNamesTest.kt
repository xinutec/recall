package org.recall.mic

import org.junit.Assert.assertEquals
import org.junit.Test
import java.time.Instant

class SegmentNamesTest {
    @Test
    fun `a segment is named by its source and UTC open instant`() {
        val name = SegmentNames.segmentName("pixel5", Instant.parse("2026-09-05T12:00:00Z"))
        assertEquals("pixel5-20260905T120000.phone.flac", name)
    }

    @Test
    fun `the stamp is UTC whatever the device zone thinks`() {
        // 23:30Z is the next local day in most of Europe; the name must not care.
        val name = SegmentNames.segmentName("usb", Instant.parse("2026-12-31T23:30:00Z"))
        assertEquals("usb-20261231T233000.phone.flac", name)
    }

    @Test
    fun `one minute of PCM is the rotation boundary`() {
        assertEquals(60 * 48_000 * 2, SegmentNames.SEGMENT_BYTES)
    }
}
