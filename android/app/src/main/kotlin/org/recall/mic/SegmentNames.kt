package org.recall.mic

import java.time.Instant
import java.time.ZoneOffset
import java.time.format.DateTimeFormatter

/**
 * The store-and-forward segment grammar (recall/docs/architecture.md): a closed
 * segment is `<source>-YYYYMMDDTHHMMSS.phone.flac`, stamped in UTC from this
 * device's clock at the moment the segment opens. The name is the only timing
 * metadata a segment carries, so it is derived in exactly one place.
 *
 * `.phone` because the host also cuts this phone's stream into FLAC, often
 * opening in the same second: without it the two copies would share a name.
 */
object SegmentNames {
    const val EXT = "phone.flac"
    const val SAMPLE_RATE = 48_000
    const val CHANNELS = 1
    const val BYTES_PER_SAMPLE = 2

    /** One minute of PCM — the rotation boundary, counted in bytes fed so the
     * writer needs no timer: audio time, not wall time, decides. */
    const val SEGMENT_BYTES = 60 * SAMPLE_RATE * BYTES_PER_SAMPLE * CHANNELS

    private val stamp: DateTimeFormatter =
        DateTimeFormatter.ofPattern("yyyyMMdd'T'HHmmss").withZone(ZoneOffset.UTC)

    fun segmentName(source: String, openedAt: Instant, ext: String = EXT): String =
        "$source-${stamp.format(openedAt)}.$ext"
}
