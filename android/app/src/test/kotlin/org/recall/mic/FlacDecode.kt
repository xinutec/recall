package org.recall.mic

import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import java.security.MessageDigest

/** Reads back what [Flac] writes, failing on any wrong CRC or MD5. */
internal object FlacDecode {
    fun bytesOf(pcm: ShortArray): ByteArray =
        ByteArray(pcm.size * 2).also { b ->
            pcm.forEachIndexed { i, s ->
                b[2 * i] = s.toByte()
                b[2 * i + 1] = (s.toInt() shr 8).toByte()
            }
        }

    fun samples(bytes: ByteArray): ShortArray {
        val r = Reader(bytes)
        assertEquals("fLaC", String(bytes, 0, 4, Charsets.US_ASCII))
        r.skip(32 + 1 + 7 + 24 + 16 + 16 + 24 + 24)
        assertEquals(48_000L, r.bits(20))
        assertEquals(0L, r.bits(3))
        assertEquals(15L, r.bits(5))
        val total = r.bits(36)
        val md5 = ByteArray(16) { r.bits(8).toByte() }
        val out = ArrayList<Short>()
        var frame = 0L
        while (r.byte < bytes.size) {
            val start = r.byte
            assertEquals(0b11111111111110L, r.bits(14))
            r.bits(2)
            val sizeCode = r.bits(4).toInt()
            r.bits(4)
            assertEquals(0L, r.bits(4))
            assertEquals(4L, r.bits(3))
            r.bits(1)
            assertEquals(frame++, r.utf8())
            val n =
                when (sizeCode) {
                    12 -> Flac.BLOCK
                    6 -> r.bits(8).toInt() + 1
                    7 -> r.bits(16).toInt() + 1
                    else -> error("block size code $sizeCode")
                }
            assertEquals(Flac.crc8(bytes.copyOfRange(start, r.byte)).toLong(), r.bits(8))
            val x = IntArray(n)
            assertEquals(0L, r.bits(1))
            val type = r.bits(6).toInt()
            assertEquals(0L, r.bits(1))
            when {
                type == 0 -> {
                    val v = r.signed(16)
                    x.fill(v)
                }

                type == 1 -> {
                    for (i in 0 until n) x[i] = r.signed(16)
                }

                type in 8..12 -> {
                    val order = type - 8
                    for (i in 0 until order) x[i] = r.signed(16)
                    assertEquals(0L, r.bits(2))
                    val po = r.bits(4).toInt()
                    var at = order
                    for (p in 0 until (1 shl po)) {
                        val k = r.bits(4).toInt()
                        val size = (n shr po) - if (p == 0) order else 0
                        repeat(size) {
                            var q = 0
                            while (r.bits(1) == 0L) q++
                            val u = (q shl k) or r.bits(k).toInt()
                            val res = (u ushr 1) xor -(u and 1)
                            x[at] = res + predict(x, at, order)
                            at++
                        }
                    }
                }

                else -> {
                    error("subframe type $type")
                }
            }
            r.alignToByte()
            assertEquals(Flac.crc16(bytes.copyOfRange(start, r.byte)).toLong(), r.bits(16))
            x.forEach { out.add(it.toShort()) }
        }
        val samples = out.toShortArray()
        if (total != 0L) {
            assertEquals(total, samples.size.toLong())
            assertArrayEquals(md5, MessageDigest.getInstance("MD5").digest(bytesOf(samples)))
        }
        return samples
    }

    private fun predict(x: IntArray, i: Int, order: Int): Int =
        when (order) {
            0 -> 0
            1 -> x[i - 1]
            2 -> 2 * x[i - 1] - x[i - 2]
            3 -> 3 * x[i - 1] - 3 * x[i - 2] + x[i - 3]
            else -> 4 * x[i - 1] - 6 * x[i - 2] + 4 * x[i - 3] - x[i - 4]
        }

    private class Reader(
        private val b: ByteArray,
    ) {
        var byte = 0
        private var bit = 0

        fun bits(n: Int): Long {
            var v = 0L
            repeat(n) {
                v = (v shl 1) or ((b[byte].toInt() shr (7 - bit)) and 1).toLong()
                if (++bit == 8) {
                    bit = 0
                    byte++
                }
            }
            return v
        }

        fun signed(n: Int): Int = (bits(n) shl (64 - n) shr (64 - n)).toInt()

        fun skip(n: Int) {
            bits(n)
        }

        fun alignToByte() {
            if (bit != 0) bits(8 - bit)
        }

        fun utf8(): Long {
            val first = bits(8).toInt()
            if (first < 0x80) return first.toLong()
            var extra = 0
            while (first and (0x40 shr extra) != 0) extra++
            var v = (first and (0x3F shr extra)).toLong()
            repeat(extra) { v = (v shl 6) or (bits(8) and 0x3F) }
            return v
        }
    }
}
