package org.recall.mic

import java.io.File
import java.io.RandomAccessFile
import java.time.Instant

/**
 * Writes the mic loop's PCM to closed, capture-stamped FLAC segments in
 * [SegmentStore], beside the live stream, for [SegmentUpload] to deliver.
 *
 * [offer] only fills a bounded [PcmSpool], so the mic is never blocked; a writer
 * thread drains it and starts a new file every [SegmentNames.SEGMENT_BYTES] of audio
 * (a minute, counted in audio, not wall time). Nothing here throws into capture: a
 * dead writer costs the copies, not the stream.
 *
 * Plain JVM (files, bytes, an injected clock), so it is tested without a device.
 */
class SegmentWriter(
    private val base: File,
    private val source: String,
    private val now: () -> Instant = Instant::now,
    spoolBytes: Int = DEFAULT_SPOOL_BYTES,
    // Not android.util.Log, to stay plain JVM.
    private val onFailure: (String) -> Unit = {},
    // On the writer thread, per closed segment; the service starts an upload.
    private val onSegmentClosed: () -> Unit = {},
) {
    private val spool = PcmSpool(spoolBytes)

    @Volatile private var running = false
    private var thread: Thread? = null

    private var file: RandomAccessFile? = null
    private var flac: FlacFile? = null
    private var path: File? = null
    private var written = 0

    /** Never blocks; when full, the spool drops its oldest bytes and counts them. */
    fun offer(chunk: ByteArray, length: Int) {
        spool.offer(chunk, length)
    }

    fun droppedBytes(): Long = spool.dropped()

    fun start() {
        if (running) return
        running = true
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
            // The spool's tail.
            write(spool.drain())
        } catch (e: Exception) {
            onFailure("segment writer failed: ${e.message}")
        } finally {
            runCatching { finishSegment() }
        }
    }

    private fun write(bytes: ByteArray) {
        var from = 0
        while (from < bytes.size) {
            val out = flac ?: openSegment()
            val room = SegmentNames.SEGMENT_BYTES - written
            val take = minOf(room, bytes.size - from)
            out.write(bytes, from, take)
            written += take
            from += take
            if (written >= SegmentNames.SEGMENT_BYTES) finishSegment()
        }
    }

    private fun openSegment(): FlacFile {
        val name = SegmentNames.segmentName(source, now())
        val target = File(SegmentStore.open(base), name)
        val out = RandomAccessFile(target, "rw")
        val encoder = FlacFile(out, SegmentNames.SAMPLE_RATE)
        file = out
        flac = encoder
        path = target
        written = 0
        return encoder
    }

    private fun finishSegment() {
        val out = file ?: return
        val encoder = flac ?: return
        val target = path ?: return
        file = null
        flac = null
        path = null
        // Finish the header, sync, then rename into the closed set: the only
        // step the uploader can see.
        encoder.finish()
        out.fd.sync()
        out.close()
        if (written == 0) {
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
