package org.recall.mic

import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import kotlin.concurrent.thread

/**
 * The handover from continuous streaming to a deliberate meeting recording.
 *
 * ⚠ MeetingService called StreamService.stop() and then IMMEDIATELY opened a
 * MediaRecorder. stopService is ASYNC: onDestroy clears `running`, and only on
 * its next iteration does the capture thread reach the finally block that runs
 * record.stop()/release(). So while continuous capture was RUNNING there was a
 * window where the old AudioRecord was still open — both audio sources failed and
 * the user was told "check permission / other apps", which sent them into Android
 * settings to look for a race in our own code.
 */
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

    /**
     * ⚠ The bound matters as much as the wait. A capture thread wedged on a dead
     * socket would otherwise hang the deliberate act forever, and a meeting the
     * user pressed record on is worth more than a tidy handover.
     */
    @Test
    fun `a microphone that is never released gives up rather than hanging`() {
        MicHandover.acquired()

        val started = System.nanoTime()
        val handed = MicHandover.awaitRelease(timeoutMs = 100)
        val tookMs = (System.nanoTime() - started) / 1_000_000

        assertFalse("it must report the handover did NOT complete", handed)
        assertTrue("returned in ${tookMs}ms, before its own bound", tookMs >= 100)
        assertTrue("took ${tookMs}ms — that is a hang, not a bound", tookMs < 2_000)
    }

    /**
     * ⚠ THE MESSAGE IS HALF THE BUG. Telling somebody to check permissions when
     * we know our own stream still held the mic is worse than saying nothing: it
     * sends them somewhere the fault has never been.
     */
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
