package org.recall.mic

import java.io.File
import java.io.RandomAccessFile
import java.time.Instant

/**
 * Turns the mic loop's PCM chunks into closed, capture-stamped WAV segments in
 * [SegmentStore] — the phone's half of store-and-forward
 * (recall/docs/architecture.md, stage C1), running in SHADOW: the PCM stream to
 * the recorder host is untouched, this only adds durable local copies that the
 * uploader delivers with verified receipts.
 *
 * The mic loop calls [offer], which hands bytes to a bounded [PcmSpool] and
 * returns — the same never-block-the-microphone rule as the network sender,
 * for the same measured reason. A writer thread drains the spool to disk and
 * rotates files every [SegmentNames.SEGMENT_BYTES] of audio (one minute of
 * PCM, counted in bytes: audio time, not wall time). Nothing here may throw
 * into the capture path; a dead writer costs the shadow copies, never the
 * stream.
 *
 * Pure JVM by construction (files, bytes, an injected clock), so rotation and
 * naming are unit-tested without a device.
 */
class SegmentWriter(
    private val base: File,
    private val source: String,
    private val now: () -> Instant = Instant::now,
    spoolBytes: Int = DEFAULT_SPOOL_BYTES,
    // Injected, not android.util.Log: the class stays pure JVM so its
    // rotation arithmetic is tested without a device; the service passes a
    // real logger.
    private val onFailure: (String) -> Unit = {},
    // Fires (on the writer thread) each time a segment lands in the closed
    // set — the service kicks the uploader here, so delivery tracks rotation
    // instead of waiting for the stream cycle to end.
    private val onSegmentClosed: () -> Unit = {},
) {
    private val spool = PcmSpool(spoolBytes)

    @Volatile private var running = false
    private var thread: Thread? = null

    private var file: RandomAccessFile? = null
    private var path: File? = null
    private var written = 0

    /** Never blocks; drops-oldest into the counted spool under pressure. */
    fun offer(chunk: ByteArray, length: Int) {
        spool.offer(chunk, length)
    }

    fun droppedBytes(): Long = spool.dropped()

    fun start() {
        if (running) return
        running = true
        // Whatever a crash left half-written is real audio: close it out first.
        SegmentStore.sweepOpen(base)
        thread =
            Thread({ drainLoop() }, "segment-writer").apply {
                isDaemon = true
                start()
            }
    }

    /** Flush, finalize the open segment, and stop. Idempotent. */
    fun close() {
        if (!running) return
        running = false
        thread?.join(JOIN_TIMEOUT_MS)
        thread = null
    }

    private fun drainLoop() {
        try {
            while (running) {
                val pending = spool.drain()
                if (pending.isEmpty()) {
                    Thread.sleep(IDLE_MS)
                } else {
                    write(pending)
                }
            }
            // One last drain so Stop does not orphan the tail of the spool.
            write(spool.drain())
        } catch (e: Exception) {
            // The shadow must never take the stream down with it.
            onFailure("segment writer failed: ${e.message}")
        } finally {
            runCatching { finishSegment() }
        }
    }

    private fun write(bytes: ByteArray) {
        var from = 0
        while (from < bytes.size) {
            val out = file ?: openSegment()
            val room = SegmentNames.SEGMENT_BYTES - written
            val take = minOf(room, bytes.size - from)
            out.write(bytes, from, take)
            written += take
            from += take
            if (written >= SegmentNames.SEGMENT_BYTES) finishSegment()
        }
    }

    private fun openSegment(): RandomAccessFile {
        val name = SegmentNames.segmentName(source, now())
        val target = File(SegmentStore.open(base), name)
        val out = RandomAccessFile(target, "rw")
        // Sizes lie zero until close; decoders read to EOF, so a crash keeps
        // a decodable file rather than headerless bytes.
        out.write(SegmentNames.wavHeader(0))
        file = out
        path = target
        written = 0
        return out
    }

    private fun finishSegment() {
        val out = file ?: return
        val target = path ?: return
        file = null
        path = null
        // Patch the header with the truth, land the bytes, then RENAME into
        // the closed set — the only step anything downstream can observe.
        out.seek(0)
        out.write(SegmentNames.wavHeader(written))
        out.fd.sync()
        out.close()
        if (written == 0) {
            // A zero-length segment says nothing; do not ship an empty claim.
            target.delete()
        } else if (target.renameTo(File(SegmentStore.root(base), target.name))) {
            onSegmentClosed()
        }
        written = 0
    }

    companion object {
        /** ~4 s of PCM headroom between mic and disk. */
        const val DEFAULT_SPOOL_BYTES =
            4 * SegmentNames.SAMPLE_RATE * SegmentNames.BYTES_PER_SAMPLE
        private const val IDLE_MS = 50L
        private const val JOIN_TIMEOUT_MS = 3_000L
    }
}
