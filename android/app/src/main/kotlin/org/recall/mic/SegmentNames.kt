package org.recall.mic

import java.time.Instant
import java.time.ZoneOffset
import java.time.format.DateTimeFormatter

/**
 * The store-and-forward segment grammar (recall/docs/architecture.md): a closed
 * segment is `<source>-YYYYMMDDTHHMMSS.<ext>`, stamped in UTC from THIS device's
 * clock at the moment the segment opens. The name is the only timing metadata a
 * segment carries, so it is derived in exactly one place.
 *
 * WAV first, deliberately: the delivery protocol is container-agnostic and a
 * recorder flips formats independently — FLAC arrives once MediaCodec's header
 * behaviour has been probed on-device rather than assumed (a plausible codec
 * shape is not a verified one).
 */
object SegmentNames {
    const val WAV_EXT = "wav"
    const val SAMPLE_RATE = 48_000
    const val CHANNELS = 1
    const val BYTES_PER_SAMPLE = 2

    /** One minute of PCM — the rotation boundary, counted in bytes fed so the
     * writer needs no timer: audio time, not wall time, decides. */
    const val SEGMENT_BYTES = 60 * SAMPLE_RATE * BYTES_PER_SAMPLE * CHANNELS

    private val stamp: DateTimeFormatter =
        DateTimeFormatter.ofPattern("yyyyMMdd'T'HHmmss").withZone(ZoneOffset.UTC)

    fun segmentName(source: String, openedAt: Instant, ext: String = WAV_EXT): String =
        "$source-${stamp.format(openedAt)}.$ext"

    /**
     * A canonical 44-byte PCM WAV header for [dataBytes] of s16le audio.
     *
     * Written twice per segment: once at open with the size fields zero, and
     * again at close with the truth — so a crash mid-segment leaves a header
     * whose sizes lie small, which decoders read to EOF anyway, rather than no
     * header at all.
     */
    fun wavHeader(dataBytes: Int): ByteArray {
        val byteRate = SAMPLE_RATE * CHANNELS * BYTES_PER_SAMPLE
        val blockAlign = CHANNELS * BYTES_PER_SAMPLE
        val header = ByteArray(44)

        fun putAscii(offset: Int, text: String) {
            text.forEachIndexed { i, c -> header[offset + i] = c.code.toByte() }
        }

        fun putLe32(offset: Int, value: Int) {
            header[offset] = (value and 0xFF).toByte()
            header[offset + 1] = ((value shr 8) and 0xFF).toByte()
            header[offset + 2] = ((value shr 16) and 0xFF).toByte()
            header[offset + 3] = ((value shr 24) and 0xFF).toByte()
        }

        fun putLe16(offset: Int, value: Int) {
            header[offset] = (value and 0xFF).toByte()
            header[offset + 1] = ((value shr 8) and 0xFF).toByte()
        }
        putAscii(0, "RIFF")
        putLe32(4, 36 + dataBytes)
        putAscii(8, "WAVE")
        putAscii(12, "fmt ")
        putLe32(16, 16) // PCM fmt chunk size
        putLe16(20, 1) // PCM
        putLe16(22, CHANNELS)
        putLe32(24, SAMPLE_RATE)
        putLe32(28, byteRate)
        putLe16(32, blockAlign)
        putLe16(34, BYTES_PER_SAMPLE * 8)
        putAscii(36, "data")
        putLe32(40, dataBytes)
        return header
    }
}
