package org.recall.mic

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class CaptureApiTest {
    @Test
    fun parsesTheSpecVsStatusShape() {
        // The transitioning answer a pause press gets: desired flipped, the mic's
        // confirmed word unchanged — rendered as "Pausing…", never a flap.
        val state =
            parseCaptureState(
                """{"running": true, "pausedUntil": null,
                    "desiredRunning": false,
                    "desiredPausedUntil": "2026-07-17T15:13:32+00:00",
                    "settled": false, "micReachable": true}""",
            )!!
        assertEquals(true, state.running)
        assertNull(state.pausedUntil)
        assertEquals(false, state.desiredRunning)
        assertEquals("2026-07-17T15:13:32+00:00", state.desiredPausedUntil)
        assertEquals(false, state.settled)
        assertEquals(true, state.micReachable)
    }

    @Test
    fun anOlderServersConfirmedOnlyAnswerReadsAsSettled() {
        // Rollout order safety: the app may meet a server that only sends the old
        // two-field shape. That is a settled state (desired == confirmed).
        val state = parseCaptureState("""{"running": false, "pausedUntil": "2026-07-17T15:13:32+00:00"}""")!!
        assertEquals(false, state.running)
        assertEquals(false, state.desiredRunning)
        assertEquals("2026-07-17T15:13:32+00:00", state.desiredPausedUntil)
        assertEquals(true, state.settled)
        assertEquals(true, state.micReachable)
    }

    @Test
    fun presentButNullDesiredPauseMeansDesiredRunning() {
        val state =
            parseCaptureState(
                """{"running": false, "pausedUntil": "2026-07-17T15:13:32+00:00",
                    "desiredRunning": true, "desiredPausedUntil": null,
                    "settled": false, "micReachable": true}""",
            )!!
        assertNull(state.desiredPausedUntil) // resuming: the target has no resume-by
        assertEquals(true, state.desiredRunning)
    }

    @Test
    fun malformedJsonIsNullNotACrash() {
        assertNull(parseCaptureState("not json"))
    }
}
