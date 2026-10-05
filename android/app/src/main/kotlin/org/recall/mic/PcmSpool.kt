package org.recall.mic

/**
 * A bounded PCM hand-off from the microphone to a writer. Capture must never wait:
 * a blocked write stops `AudioRecord.read`, and its ~1 s buffer overruns.
 *
 * So `offer` never blocks: when full, the oldest audio is dropped and counted, since
 * the newest is likelier to be looked for. Synchronised, plain JVM.
 */
class PcmSpool(
    private val capacityBytes: Int,
) {
    private val buffer = ByteArray(capacityBytes)
    private var start = 0
    private var count = 0
    private var droppedBytes = 0L

    /** Take `length` bytes of PCM. Never blocks. */
    @Synchronized
    fun offer(chunk: ByteArray, length: Int) {
        // A chunk bigger than the spool keeps only its tail.
        val from = if (length > capacityBytes) length - capacityBytes else 0
        val take = length - from
        val overflow = (count + take) - capacityBytes
        if (overflow > 0) discardOldest(overflow)
        droppedBytes += from.toLong()
        for (i in 0 until take) {
            buffer[(start + count) % capacityBytes] = chunk[from + i]
            count++
        }
    }

    /** Everything held, oldest first, emptying the spool. */
    @Synchronized
    fun drain(): ByteArray {
        val out = ByteArray(count)
        for (i in 0 until count) out[i] = buffer[(start + i) % capacityBytes]
        start = 0
        count = 0
        return out
    }

    /** Bytes currently waiting to be sent. */
    @Synchronized fun size(): Int = count

    /** Bytes of captured audio discarded because the sender could not keep up. */
    @Synchronized fun dropped(): Long = droppedBytes

    private fun discardOldest(bytes: Int) {
        val drop = minOf(bytes, count)
        start = (start + drop) % capacityBytes
        count -= drop
        droppedBytes += drop.toLong()
    }
}
