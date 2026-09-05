package org.recall.mic

import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder
import java.time.Instant

class SegmentWriterTest {
    @get:Rule
    val tmp = TemporaryFolder()

    @Test
    fun `pcm rotates into capture-stamped closed segments`() {
        val instants =
            ArrayDeque(
                listOf(
                    Instant.parse("2026-09-05T12:00:00Z"),
                    Instant.parse("2026-09-05T12:01:00Z"),
                ),
            )
        // A spool that holds the whole feed makes the test deterministic:
        // everything offered before close() must land, drop-free, whatever
        // the writer thread's scheduling did.
        val writer =
            SegmentWriter(
                tmp.root,
                "pixel5",
                now = { instants.removeFirst() },
                spoolBytes = SegmentNames.SEGMENT_BYTES + 100_000,
            )
        writer.start()
        // One full segment plus one second of the next.
        val full = ByteArray(SegmentNames.SEGMENT_BYTES) { (it % 251).toByte() }
        val tail = ByteArray(96_000) { 7 }
        writer.offer(full, full.size)
        writer.offer(tail, tail.size)
        writer.close()
        assertEquals(0L, writer.droppedBytes())
        val closed = SegmentStore.undelivered(tmp.root)
        assertEquals(
            listOf("pixel5-20260905T120000.wav", "pixel5-20260905T120100.wav"),
            closed.map { it.name },
        )
        // First file: header + exactly one minute of the fed pattern.
        val first = closed[0].readBytes()
        assertEquals(44 + SegmentNames.SEGMENT_BYTES, first.size)
        assertArrayEquals(
            SegmentNames.wavHeader(SegmentNames.SEGMENT_BYTES),
            first.copyOfRange(0, 44),
        )
        assertEquals(full[0], first[44])
        assertEquals(full[SegmentNames.SEGMENT_BYTES - 1], first[first.size - 1])
        // Second: the one-second tail, closed by close() with a truthful header.
        val second = closed[1].readBytes()
        assertEquals(44 + 96_000, second.size)
        assertArrayEquals(SegmentNames.wavHeader(96_000), second.copyOfRange(0, 44))
        assertTrue(second.drop(44).all { it == 7.toByte() })
    }

    @Test
    fun `closing with nothing recorded ships no empty claim`() {
        val writer =
            SegmentWriter(tmp.root, "pixel5", now = { Instant.parse("2026-09-05T12:00:00Z") })
        writer.start()
        writer.close()
        assertTrue(SegmentStore.undelivered(tmp.root).isEmpty())
    }

    @Test
    fun `each closed segment is announced`() {
        var announced = 0
        val writer =
            SegmentWriter(
                tmp.root,
                "pixel5",
                now = { Instant.parse("2026-09-05T12:00:00Z") },
                onSegmentClosed = { announced += 1 },
            )
        writer.start()
        val second = ByteArray(96_000) { 1 }
        writer.offer(second, second.size)
        writer.close()
        assertEquals(1, announced)
    }
}
