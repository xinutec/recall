package org.recall.mic

import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class PcmSpoolTest {
    @Test
    fun deliversWhatWasCapturedInOrder() {
        val spool = PcmSpool(capacityBytes = 64)
        spool.offer(byteArrayOf(1, 2, 3), 3)
        spool.offer(byteArrayOf(4, 5), 2)
        assertArrayEquals(byteArrayOf(1, 2, 3, 4, 5), spool.drain())
        assertEquals(0, spool.dropped())
    }

    @Test
    fun captureNeverBlocksWhenTheSenderStalls() {
        // The whole point (#1330/#1365 follow-up): a stalled Mac must never stop the
        // phone reading its own microphone. Offering past capacity returns at once.
        val spool = PcmSpool(capacityBytes = 8)
        repeat(100) { spool.offer(ByteArray(4) { 7 }, 4) }
        assertTrue("spool stays bounded", spool.size() <= 8)
    }

    @Test
    fun anOverrunDropsTheOLDESTAudioAndSaysHowMuch() {
        // Drops are counted, since only the phone knows. Oldest first: the newest
        // speech is the most likely to be looked for.
        val spool = PcmSpool(capacityBytes = 4)
        spool.offer(byteArrayOf(1, 2, 3, 4), 4)
        spool.offer(byteArrayOf(5, 6), 2)
        assertArrayEquals(byteArrayOf(3, 4, 5, 6), spool.drain())
        assertEquals(2, spool.dropped())
    }

    @Test
    fun drainEmptiesSoTheNextDrainSeesOnlyNewAudio() {
        val spool = PcmSpool(capacityBytes = 64)
        spool.offer(byteArrayOf(1, 2), 2)
        spool.drain()
        spool.offer(byteArrayOf(3), 1)
        assertArrayEquals(byteArrayOf(3), spool.drain())
    }

    @Test
    fun aChunkLargerThanTheWholeSpoolKeepsItsTail() {
        val spool = PcmSpool(capacityBytes = 3)
        spool.offer(byteArrayOf(1, 2, 3, 4, 5), 5)
        assertArrayEquals(byteArrayOf(3, 4, 5), spool.drain())
        assertEquals(2, spool.dropped())
    }
}
