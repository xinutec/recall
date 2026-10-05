package org.recall.mic

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class CaptureApiTest {
    @Test
    fun parsesTheSpecVsStatusShape() {
        // The answer to a pause press: desired changed, confirmed not yet.
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
        val state =
            parseCaptureState(
                """{"running": false, "pausedUntil": "2026-07-17T15:13:32+00:00"}""",
            )!!
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

    @Test
    fun parsesTheLongPollStateToken() {
        val state =
            parseCaptureState(
                """{"running": true, "pausedUntil": null, "stateToken": "abc123def456"}""",
            )!!
        assertEquals("abc123def456", state.stateToken)
    }

    @Test
    fun anOlderServerWithoutATokenReadsAsNull() {
        val state = parseCaptureState("""{"running": true, "pausedUntil": null}""")!!
        assertNull(state.stateToken)
    }
}
