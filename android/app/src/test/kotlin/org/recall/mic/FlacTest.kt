package org.recall.mic

import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder
import java.io.RandomAccessFile
import kotlin.math.PI
import kotlin.math.sin
import kotlin.random.Random

/** Lossless means the decoded samples are the fed ones, bit for bit. */
class FlacTest {
    @get:Rule val tmp = TemporaryFolder()

    @Test
    fun `speech-like audio decodes to the same samples`() {
        val rng = Random(7)
        val pcm =
            ShortArray(48_000 * 3) {
                (8000 * sin(2 * PI * 220 * it / 48_000) + rng.nextInt(-300, 300)).toInt().toShort()
            }
        roundTrip(pcm)
    }

    @Test
    fun `silence, full-scale noise and a short tail decode exactly`() {
        val rng = Random(11)
        roundTrip(ShortArray(Flac.BLOCK * 2 + 300))
        roundTrip(ShortArray(Flac.BLOCK + 17) { rng.nextInt(-32768, 32768).toShort() })
        roundTrip(ShortArray(3) { (it * 1000).toShort() })
        roundTrip(
            shortArrayOf(Short.MIN_VALUE, Short.MAX_VALUE, Short.MIN_VALUE, Short.MAX_VALUE, 0),
        )
    }

    @Test
    fun `a sample split across two writes is put back together`() {
        val pcm = ShortArray(5000) { (it * 7 - 9000).toShort() }
        val bytes = FlacDecode.bytesOf(pcm)
        val file = tmp.newFile()
        RandomAccessFile(file, "rw").use { out ->
            val flac = FlacFile(out, 48_000)
            var at = 0
            for (size in generateSequence(1) { it % 7 + 1 }) {
                if (at >= bytes.size) break
                val take = minOf(size, bytes.size - at)
                flac.write(bytes, at, take)
                at += take
            }
            flac.finish()
        }
        assertArrayEquals(pcm, FlacDecode.samples(file.readBytes()))
    }

    @Test
    fun `a minute of speech-like audio is far smaller than its WAV`() {
        val rng = Random(3)
        val pcm =
            ShortArray(48_000 * 60) {
                (3000 * sin(2 * PI * 180 * it / 48_000) + rng.nextInt(-20, 20)).toInt().toShort()
            }
        val size = encode(pcm).size
        assertTrue("$size bytes", size < pcm.size * 2 / 2)
    }

    @Test
    fun `a file cut short still decodes what it holds`() {
        val pcm = ShortArray(Flac.BLOCK * 3) { (it % 500 - 250).toShort() }
        val file = tmp.newFile()
        RandomAccessFile(file, "rw").use { out ->
            // No finish: the header still says "length unknown".
            FlacFile(out, 48_000).write(FlacDecode.bytesOf(pcm), 0, pcm.size * 2)
        }
        assertArrayEquals(pcm, FlacDecode.samples(file.readBytes()))
    }

    private fun roundTrip(pcm: ShortArray) {
        val decoded = FlacDecode.samples(encode(pcm))
        assertArrayEquals(pcm, decoded)
    }

    private fun encode(pcm: ShortArray): ByteArray {
        val file = tmp.newFile()
        RandomAccessFile(file, "rw").use { out ->
            val flac = FlacFile(out, 48_000)
            val bytes = FlacDecode.bytesOf(pcm)
            flac.write(bytes, 0, bytes.size)
            flac.finish()
        }
        return file.readBytes()
    }
}
