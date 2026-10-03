package org.recall.mic

/**
 * A bounded PCM hand-off between the microphone and the network.
 *
 * Capture must never wait for the network. A socket write blocked by TCP
 * backpressure stops `AudioRecord.read`, the device's ~1 s buffer overruns, and the
 * phone drops speech it has already heard.
 *
 * So `offer` never blocks and never fails: if the spool fills, the oldest audio is
 * discarded and counted. Dropping is a loss either way; here it is bounded, chosen
 * and reported instead of invisible inside a device buffer.
 *
 * Oldest-first because in a memory aid the newest speech is what someone is most
 * likely to come looking for. Pure and synchronised, so it is unit-tested on the
 * JVM without a device.
 */
class PcmSpool(
    private val capacityBytes: Int,
) {
    private val buffer = ByteArray(capacityBytes)
    private var start = 0
    private var count = 0
    private var droppedBytes = 0L

    /** Take `length` bytes of freshly-captured PCM. Never blocks. */
    @Synchronized
    fun offer(chunk: ByteArray, length: Int) {
        // A chunk bigger than the whole spool keeps only its tail — the newest audio.
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
