package org.recall.mic

import java.io.File

/**
 * The phone's segment cache: where closed segments wait, and what each
 * directory MEANS — the meeting queue's idiom, because every state here is a
 * verdict that must survive a reboot and a rename is the only change that
 * cannot half-happen:
 *
 * | directory              | meaning                                             |
 * |------------------------|-----------------------------------------------------|
 * | `segments/open/`       | being written by the recorder, nothing touches it   |
 * | `segments/`            | closed, undelivered — what the uploader drains      |
 * | `segments/delivered/`  | Isis holds it, PROVEN: the receipt's sha-256 matched a local re-hash |
 * | `segments/conflict/`   | Isis holds DIFFERENT bytes under this name — a person must look |
 *
 * Deletion happens in exactly one place, [evict], and eats only `delivered/`,
 * oldest first, under cache pressure — never an undelivered segment, never a
 * conflict, never because a server asked (recall/docs/architecture.md,
 * decision 2: eviction is a local decision; Isis's word destroys nothing).
 */
object SegmentStore {
    private const val DIR = "segments"
    private const val OPEN = "open"
    private const val DELIVERED = "delivered"
    private const val CONFLICT = "conflict"

    /** ~2 GiB: hours of WAV, days of FLAC — enough local history to span the
     * upload→nightly-backup window that makes eviction safe at all. */
    const val DEFAULT_CEILING_BYTES = 2L * 1024 * 1024 * 1024

    fun root(base: File): File = File(base, DIR).apply { mkdirs() }

    fun open(base: File): File = File(root(base), OPEN).apply { mkdirs() }

    fun delivered(base: File): File = File(root(base), DELIVERED).apply { mkdirs() }

    fun conflict(base: File): File = File(root(base), CONFLICT).apply { mkdirs() }

    private fun segmentsIn(dir: File): List<File> =
        (dir.listFiles() ?: emptyArray()).filter { it.isFile }.sortedBy { it.name }

    /** Closed, undelivered, oldest first — the uploader's work list. */
    fun undelivered(base: File): List<File> = segmentsIn(root(base))

    /**
     * Adopt anything a crash left in `open/`: a truncated segment is real
     * audio that was really heard — completeness outranks tidiness, so it
     * closes as-is and ships. Run once at recorder start, before a new
     * segment opens.
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
     * Free space down to [ceilingBytes], eating verified-delivered segments
     * oldest first and NOTHING else. Returns how many were evicted. If the
     * cache is over the ceiling with `delivered/` empty, it stays over — the
     * undelivered audio is the point of the cache, and deleting it to meet a
     * number would be the exact loss this design exists to prevent.
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
