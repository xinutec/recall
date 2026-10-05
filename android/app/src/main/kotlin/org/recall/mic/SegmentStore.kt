package org.recall.mic

import java.io.File

/**
 * The phone's segment cache. As in [MeetingQueue], a segment's state is its
 * directory, since a rename cannot half-happen:
 *
 * | directory              | meaning                                             |
 * |------------------------|-----------------------------------------------------|
 * | `segments/open/`       | being written                                       |
 * | `segments/`            | closed, undelivered: what the uploader drains       |
 * | `segments/delivered/`  | on Isis, its receipt's sha-256 matching ours        |
 * | `segments/conflict/`   | Isis holds different bytes under the name, or refused it |
 *
 * Only [evict] deletes, and only from `delivered/`, oldest first, under cache
 * pressure; never because the server asked (docs/architecture.md, decision 2).
 */
object SegmentStore {
    private const val DIR = "segments"
    private const val OPEN = "open"
    private const val DELIVERED = "delivered"
    private const val CONFLICT = "conflict"

    /** ~2 GiB, over a day of FLAC: covers the time from upload to the server's
     * nightly backup. */
    const val DEFAULT_CEILING_BYTES = 2L * 1024 * 1024 * 1024

    fun root(base: File): File = File(base, DIR).apply { mkdirs() }

    fun open(base: File): File = File(root(base), OPEN).apply { mkdirs() }

    fun delivered(base: File): File = File(root(base), DELIVERED).apply { mkdirs() }

    fun conflict(base: File): File = File(root(base), CONFLICT).apply { mkdirs() }

    private fun segmentsIn(dir: File): List<File> =
        (dir.listFiles() ?: emptyArray()).filter { it.isFile }.sortedBy { it.name }

    /** Closed and undelivered, oldest first. */
    fun undelivered(base: File): List<File> = segmentsIn(root(base))

    /**
     * Close whatever a crash left in `open/`: truncated, but real audio. Run at
     * recorder start, before a new segment opens.
     */
    fun sweepOpen(base: File) {
        for (orphan in segmentsIn(open(base))) {
            orphan.renameTo(File(root(base), orphan.name))
        }
    }

    fun markDelivered(base: File, segment: File): Boolean =
        segment.renameTo(File(delivered(base), segment.name))

    fun markConflict(base: File, segment: File): Boolean =
        segment.renameTo(File(conflict(base), segment.name))

    private fun totalBytes(base: File): Long =
        (
            segmentsIn(root(base)) + segmentsIn(open(base)) +
                segmentsIn(delivered(base)) + segmentsIn(conflict(base))
        ).sumOf { it.length() }

    /**
     * Delete delivered segments, oldest first, until the cache is under
     * [ceilingBytes]; how many. With nothing delivered it stays over.
     */
    fun evict(base: File, ceilingBytes: Long = DEFAULT_CEILING_BYTES): Int {
        var evicted = 0
        var total = totalBytes(base)
        for (oldest in segmentsIn(delivered(base))) {
            if (total <= ceilingBytes) break
            val size = oldest.length()
            if (oldest.delete()) {
                total -= size
                evicted += 1
            }
        }
        return evicted
    }
}
