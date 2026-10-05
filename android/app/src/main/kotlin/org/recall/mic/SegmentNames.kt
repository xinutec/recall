package org.recall.mic

import java.time.Instant
import java.time.ZoneOffset
import java.time.format.DateTimeFormatter

/**
 * Segment names (docs/architecture.md): `<source>-YYYYMMDDTHHMMSS.phone.flac`, the
 * UTC time the segment opened by this phone's clock; a segment's only timing.
 *
 * `.phone`, since the host cuts the same stream into FLAC too, often starting in the
 * same second.
 */
object SegmentNames {
    const val EXT = "phone.flac"
    const val SAMPLE_RATE = 48_000
    const val CHANNELS = 1
    const val BYTES_PER_SAMPLE = 2

    /** A minute of PCM: segments are cut by audio, not wall time. */
    const val SEGMENT_BYTES = 60 * SAMPLE_RATE * BYTES_PER_SAMPLE * CHANNELS

    private val stamp: DateTimeFormatter =
        DateTimeFormatter.ofPattern("yyyyMMdd'T'HHmmss").withZone(ZoneOffset.UTC)

    fun segmentName(source: String, openedAt: Instant, ext: String = EXT): String =
        "$source-${stamp.format(openedAt)}.$ext"
}
