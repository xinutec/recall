package org.recall.mic

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder
import java.io.File

class SegmentStoreTest {
    @get:Rule
    val tmp = TemporaryFolder()

    private fun put(dir: File, name: String, bytes: Int = 10): File =
        File(dir, name).apply { writeBytes(ByteArray(bytes)) }

    @Test
    fun `a crash orphan in open closes as-is and ships`() {
        // A truncated segment is real audio that was really heard.
        val base = tmp.root
        put(SegmentStore.open(base), "pixel5-20260905T120000.wav")
        SegmentStore.sweepOpen(base)
        assertEquals(
            listOf("pixel5-20260905T120000.wav"),
            SegmentStore.undelivered(base).map { it.name },
        )
    }

    @Test
    fun `the uploader's work list is oldest first`() {
        val base = tmp.root
        put(SegmentStore.root(base), "pixel5-20260905T120100.wav")
        put(SegmentStore.root(base), "pixel5-20260905T120000.wav")
        assertEquals(
            listOf("pixel5-20260905T120000.wav", "pixel5-20260905T120100.wav"),
            SegmentStore.undelivered(base).map { it.name },
        )
    }

    @Test
    fun `delivered and conflict are renames out of the work list`() {
        val base = tmp.root
        val a = put(SegmentStore.root(base), "pixel5-20260905T120000.wav")
        val b = put(SegmentStore.root(base), "pixel5-20260905T120100.wav")
        assertTrue(SegmentStore.markDelivered(base, a))
        assertTrue(SegmentStore.markConflict(base, b))
        assertTrue(SegmentStore.undelivered(base).isEmpty())
    }

    @Test
    fun `eviction eats verified-delivered oldest first and nothing else`() {
        val base = tmp.root
        put(SegmentStore.delivered(base), "pixel5-20260905T120000.wav", 100)
        put(SegmentStore.delivered(base), "pixel5-20260905T120100.wav", 100)
        val undelivered = put(SegmentStore.root(base), "pixel5-20260905T120200.wav", 100)
        val conflicted = put(SegmentStore.conflict(base), "pixel5-20260905T120300.wav", 100)
        // 400 bytes on disk, ceiling 320: evicting the oldest delivered
        // segment (100) reaches 300 and stops — exactly one goes.
        assertEquals(1, SegmentStore.evict(base, ceilingBytes = 320))
        assertEquals(
            listOf("pixel5-20260905T120100.wav"),
            (SegmentStore.delivered(base).listFiles() ?: emptyArray()).map { it.name },
        )
        assertTrue(undelivered.exists())
        assertTrue(conflicted.exists())
    }

    @Test
    fun `over the ceiling with nothing delivered stays over rather than lose audio`() {
        val base = tmp.root
        val only = put(SegmentStore.root(base), "pixel5-20260905T120000.wav", 1000)
        assertEquals(0, SegmentStore.evict(base, ceilingBytes = 10))
        assertTrue(only.exists())
    }
}
