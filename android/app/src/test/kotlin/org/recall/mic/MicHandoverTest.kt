package org.recall.mic

import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import kotlin.concurrent.thread

/** The handover of the mic from streaming to a meeting recording ([MicHandover]). */
class MicHandoverTest {
    @After
    fun clear() = MicHandover.released()

    @Test
    fun `a free microphone hands over at once`() {
        MicHandover.released()

        assertTrue(MicHandover.awaitRelease(timeoutMs = 50))
    }

    @Test
    fun `a held microphone is waited for, not walked over`() {
        MicHandover.acquired()
        val freed =
            thread {
                Thread.sleep(60)
                MicHandover.released()
            }

        val handed = MicHandover.awaitRelease(timeoutMs = 2_000)
        freed.join()

        assertTrue("the wait must see the release the capture thread performs", handed)
    }

    @Test
    fun `a microphone that is never released gives up rather than hanging`() {
        MicHandover.acquired()

        val started = System.nanoTime()
        val handed = MicHandover.awaitRelease(timeoutMs = 100)
        val tookMs = (System.nanoTime() - started) / 1_000_000

        assertFalse("it must report the handover did not complete", handed)
        assertTrue("returned in ${tookMs}ms, before its own bound", tookMs >= 100)
        assertTrue("took ${tookMs}ms", tookMs < 2_000)
    }

    @Test
    fun `a failed handover does not blame permissions`() {
        val message = MicHandover.failureMessage(handedOver = false)

        assertFalse(message, message.contains("permission", ignoreCase = true))
        assertTrue(message, message.contains("streaming", ignoreCase = true))
    }

    @Test
    fun `a genuine mic failure after a clean handover still points at permissions`() {
        val message = MicHandover.failureMessage(handedOver = true)

        assertTrue(message, message.contains("permission", ignoreCase = true))
    }

    @Test
    fun `acquired and released track what the capture thread is doing`() {
        MicHandover.acquired()
        assertEquals(true, MicHandover.isHeld())

        MicHandover.released()
        assertEquals(false, MicHandover.isHeld())
    }
}
