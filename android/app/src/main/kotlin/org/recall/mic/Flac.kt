package org.recall.mic

import java.io.RandomAccessFile
import java.security.MessageDigest

/**
 * FLAC for 16-bit mono PCM: fixed-predictor subframes with Rice-coded residuals,
 * the method libFLAC's fastest levels use. Lossless, about a seventh of the WAV
 * on household audio. Pure JVM, so the tests prove it by decoding.
 */
object Flac {
    const val BLOCK = 4096

    /** `fLaC` and the STREAMINFO block: the bytes [FlacFile] patches at close. */
    const val HEADER_BYTES = 42

    private const val BITS = 16
    private const val MAX_ORDER = 4
    private const val MAX_PARTITION_ORDER = 6
    private const val MAX_RICE = 14

    /**
     * Zero [totalSamples], frame sizes or [md5] mean "unknown", which is what a
     * file cut short by a crash carries: decoders then read to the end.
     */
    fun header(
        rate: Int,
        totalSamples: Long,
        minFrame: Int,
        maxFrame: Int,
        md5: ByteArray,
    ): ByteArray {
        val b = Bits(HEADER_BYTES)
        for (c in "fLaC") b.put(c.code.toLong(), 8)
        b.put(1, 1) // the last metadata block
        b.put(0, 7) // STREAMINFO
        b.put(34, 24)
        b.put(BLOCK.toLong(), 16)
        b.put(BLOCK.toLong(), 16)
        b.put(minFrame.toLong(), 24)
        b.put(maxFrame.toLong(), 24)
        b.put(rate.toLong(), 20)
        b.put(0, 3) // one channel
        b.put((BITS - 1).toLong(), 5)
        b.put(totalSamples, 36)
        for (byte in md5) b.put(byte.toLong(), 8)
        return b.bytes()
    }

    /** One frame of [count] samples; only the stream's last may be short of [BLOCK]. */
    fun frame(samples: IntArray, count: Int, number: Long, rate: Int): ByteArray {
        val b = Bits(count * 2 + 64)
        b.put(0b11111111111110, 14)
        b.put(0, 2) // reserved, fixed block size
        val sizeCode =
            when {
                count == BLOCK -> 12
                count <= 256 -> 6
                else -> 7
            }
        b.put(sizeCode.toLong(), 4)
        b.put(if (rate == 48_000) 10 else 0, 4)
        b.put(0, 4) // mono
        b.put(4, 3) // 16 bits per sample
        b.put(0, 1)
        utf8(b, number)
        if (sizeCode == 6) b.put((count - 1).toLong(), 8)
        if (sizeCode == 7) b.put((count - 1).toLong(), 16)
        b.put(crc8(b.bytes()).toLong(), 8)
        subframe(b, samples, count)
        b.align()
        b.put(crc16(b.bytes()).toLong(), 16)
        return b.bytes()
    }

    private fun subframe(b: Bits, x: IntArray, n: Int) {
        if ((1 until n).all { x[it] == x[0] }) {
            b.put(0, 8) // constant
            b.put(x[0].toLong(), BITS)
            return
        }
        var best = -1
        var bestSum = Long.MAX_VALUE
        for (order in 0..minOf(MAX_ORDER, n - 1)) {
            var sum = 0L
            for (i in order until n) sum += kotlin.math.abs(residual(x, i, order).toLong())
            if (sum < bestSum) {
                bestSum = sum
                best = order
            }
        }
        val residuals = IntArray(n - best) { residual(x, it + best, best) }
        val rice = rice(residuals, n, best)
        if (8 + best * BITS + 6 + rice.bits >= 8 + n * BITS) {
            b.put(1 shl 1, 8) // verbatim
            for (i in 0 until n) b.put(x[i].toLong(), BITS)
            return
        }
        b.put(((8 or best) shl 1).toLong(), 8) // fixed, of this order
        for (i in 0 until best) b.put(x[i].toLong(), BITS)
        b.put(0, 2) // Rice, 4-bit parameters
        b.put(rice.partitionOrder.toLong(), 4)
        var at = 0
        for ((p, k) in rice.parameters.withIndex()) {
            val size = (n shr rice.partitionOrder) - if (p == 0) best else 0
            b.put(k.toLong(), 4)
            for (i in at until at + size) {
                val u = fold(residuals[i])
                b.unary(u ushr k)
                b.put(u.toLong(), k)
            }
            at += size
        }
    }

    private class Rice(
        val partitionOrder: Int,
        val parameters: IntArray,
        val bits: Long,
    )

    /** The partition order and per-partition parameters that cost the fewest bits. */
    private fun rice(residuals: IntArray, n: Int, order: Int): Rice {
        var best: Rice? = null
        for (po in 0..MAX_PARTITION_ORDER) {
            if (n % (1 shl po) != 0 || (n shr po) <= order) break
            val parameters = IntArray(1 shl po)
            var bits = 0L
            var at = 0
            for (p in parameters.indices) {
                val size = (n shr po) - if (p == 0) order else 0
                var sum = 0L
                for (i in at until at + size) sum += fold(residuals[i]).toLong()
                val guess =
                    if (size == 0 ||
                        sum < size
                    ) {
                        0
                    } else {
                        63 - java.lang.Long.numberOfLeadingZeros(sum / size)
                    }
                var partBest = Long.MAX_VALUE
                for (k in maxOf(0, guess - 1)..minOf(MAX_RICE, guess + 1)) {
                    var cost = 4L + size.toLong() * (k + 1)
                    for (i in at until at + size) cost += (fold(residuals[i]) ushr k).toLong()
                    if (cost < partBest) {
                        partBest = cost
                        parameters[p] = k
                    }
                }
                bits += partBest
                at += size
            }
            if (best == null || bits < best.bits) best = Rice(po, parameters, bits)
        }
        return best!!
    }

    private fun residual(x: IntArray, i: Int, order: Int): Int =
        when (order) {
            0 -> x[i]
            1 -> x[i] - x[i - 1]
            2 -> x[i] - 2 * x[i - 1] + x[i - 2]
            3 -> x[i] - 3 * x[i - 1] + 3 * x[i - 2] - x[i - 3]
            else -> x[i] - 4 * x[i - 1] + 6 * x[i - 2] - 4 * x[i - 3] + x[i - 4]
        }

    private fun fold(r: Int): Int = (r shl 1) xor (r shr 31)

    private fun utf8(b: Bits, v: Long) {
        if (v < 0x80) {
            b.put(v, 8)
            return
        }
        var extra = 1
        while (v >= (1L shl (5 * extra + 6))) extra++
        b.put(((0xFF00 shr (extra + 1)) and 0xFF).toLong() or (v ushr (6 * extra)), 8)
        for (i in extra - 1 downTo 0) b.put(0x80L or ((v ushr (6 * i)) and 0x3F), 8)
    }

    fun crc8(bytes: ByteArray): Int {
        var crc = 0
        for (byte in bytes) {
            crc = crc xor (byte.toInt() and 0xFF)
            repeat(8) {
                crc =
                    if (crc and 0x80 != 0) ((crc shl 1) xor 0x07) and 0xFF else (crc shl 1) and 0xFF
            }
        }
        return crc
    }

    fun crc16(bytes: ByteArray): Int {
        var crc = 0
        for (byte in bytes) {
            crc = crc xor ((byte.toInt() and 0xFF) shl 8)
            repeat(8) {
                crc =
                    if (crc and 0x8000 !=
                        0
                    ) {
                        ((crc shl 1) xor 0x8005) and 0xFFFF
                    } else {
                        (crc shl 1) and 0xFFFF
                    }
            }
        }
        return crc
    }

    /** Big-endian bits into a growing byte array. */
    private class Bits(
        capacity: Int,
    ) {
        private var buf = ByteArray(capacity)
        private var len = 0
        private var acc = 0L
        private var pending = 0

        /** The low [n] bits of [value], n at most 32. */
        fun put(value: Long, n: Int) {
            if (n == 0) return
            acc = (acc shl n) or (value and ((1L shl n) - 1))
            pending += n
            while (pending >= 8) {
                pending -= 8
                push(((acc ushr pending) and 0xFF).toInt())
            }
            acc = acc and ((1L shl pending) - 1)
        }

        fun unary(zeros: Int) {
            var left = zeros
            while (left >= 32) {
                put(0, 32)
                left -= 32
            }
            put(1, left + 1)
        }

        fun align() {
            if (pending > 0) put(0, 8 - pending)
        }

        fun bytes(): ByteArray = buf.copyOf(len)

        private fun push(byte: Int) {
            if (len == buf.size) buf = buf.copyOf(buf.size * 2)
            buf[len++] = byte.toByte()
        }
    }
}

/**
 * A FLAC file written as s16le PCM arrives: a frame lands each time a block
 * fills, and [finish] patches the header with the truth. A crash leaves the
 * frames written so far under a header that says "length unknown", which
 * decoders read to the end.
 */
class FlacFile(
    private val out: RandomAccessFile,
    private val rate: Int,
) {
    private val block = IntArray(Flac.BLOCK)
    private var filled = 0
    private var low = -1
    private val md5 = MessageDigest.getInstance("MD5")
    private var frames = 0L
    private var samples = 0L
    private var minFrame = Int.MAX_VALUE
    private var maxFrame = 0

    init {
        out.write(Flac.header(rate, 0, 0, 0, ByteArray(16)))
    }

    fun write(pcm: ByteArray, from: Int, length: Int) {
        for (i in from until from + length) {
            val byte = pcm[i].toInt() and 0xFF
            if (low < 0) {
                low = byte
                continue
            }
            block[filled++] = ((byte shl 8) or low).toShort().toInt()
            low = -1
            if (filled == Flac.BLOCK) flush()
        }
    }

    fun finish() {
        flush()
        out.seek(0)
        val known = frames > 0
        out.write(
            Flac.header(
                rate,
                samples,
                if (known) minFrame else 0,
                maxFrame,
                if (known) md5.digest() else ByteArray(16),
            ),
        )
    }

    private fun flush() {
        if (filled == 0) return
        val pcm = ByteArray(filled * 2)
        for (i in 0 until filled) {
            pcm[2 * i] = block[i].toByte()
            pcm[2 * i + 1] = (block[i] shr 8).toByte()
        }
        md5.update(pcm)
        val frame = Flac.frame(block, filled, frames, rate)
        out.write(frame)
        minFrame = minOf(minFrame, frame.size)
        maxFrame = maxOf(maxFrame, frame.size)
        frames++
        samples += filled
        filled = 0
    }
}
