package org.recall.mic

import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/** A phone with no recorder host records files on its own ([FilesOnly]). */
class FilesOnlyTest {
    private fun state(desiredRunning: Boolean) =
        CaptureState(
            running = desiredRunning,
            pausedUntil = null,
            desiredRunning = desiredRunning,
            desiredPausedUntil = null,
            settled = true,
            micReachable = true,
            stateToken = "",
        )

    @Test
    fun `no recorder host means files only`() {
        assertTrue(FilesOnly.applies(""))
        assertTrue(FilesOnly.applies("  "))
        assertFalse(FilesOnly.applies("192.168.1.10"))
    }

    @Test
    fun `recall's answer decides`() {
        assertTrue(FilesOnly.paused(last = false, answer = state(desiredRunning = false)))
        assertFalse(FilesOnly.paused(last = true, answer = state(desiredRunning = true)))
    }

    @Test
    fun `out of reach, the last known pause stands`() {
        // Away from any network the phone keeps recording, unless it was paused.
        assertFalse(FilesOnly.paused(last = false, answer = null))
        assertTrue(FilesOnly.paused(last = true, answer = null))
    }
}
