package org.recall.mic

/**
 * A bounded PCM hand-off between the microphone and the network.
 *
 * The mic loop used to write captured frames straight to the socket, so when the
 * recorder host was momentarily busy TCP backpressure blocked the write, the loop
 * stopped calling `AudioRecord.read`, and the device's own ~1s buffer overran —
 * the phone dropped speech it had already heard, before the network was even
 * involved. Measured 2026-09-03 with the Mac at load 42: the phones' segment
 * rotation slipped to 162s and 228s worst-case, in lockstep with the Mac's load.
 *
 * A recorder must not depend on its consumer's mood. `offer` never blocks and
 * never fails: capture keeps up whatever the network is doing, and if the spool
 * fills, the OLDEST audio is discarded and counted. Dropping is a real loss
 * either way — the point is that it is bounded, chosen, and REPORTED, rather
 * than happening invisibly inside a device buffer.
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
