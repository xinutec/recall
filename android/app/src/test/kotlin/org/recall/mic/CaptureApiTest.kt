package org.recall.mic

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test
import java.time.Instant

class CaptureApiTest {
    private val end = Instant.parse("2026-10-16T12:00:00Z")

    @Test
    fun parsesTheAskedVersusAppliedShape() {
        // The answer to a record press: desired changed, applied not yet.
        val state =
            parseCaptureState(
                """{"running":false,"until":null,"held":false,"desiredRunning":true,
                    "desiredUntil":"2026-10-16T12:00:00Z","desiredHeld":false,
                    "settled":false,"micReachable":true,"stateToken":"abc123def456"}""",
            )!!
        assertEquals(false, state.running)
        assertNull(state.until)
        assertEquals(true, state.desiredRunning)
        assertEquals("2026-10-16T12:00:00Z", state.desiredUntil)
        assertEquals(false, state.settled)
        assertEquals("abc123def456", state.stateToken)
        assertEquals(Window.Record(end), state.desiredWindow())
    }

    @Test
    fun eachDesiredStateNamesItsWindow() {
        fun state(running: Boolean, until: String?, held: Boolean) =
            parseCaptureState(
                """{"running":$running,"until":null,"held":false,"desiredRunning":$running,
                    "desiredUntil":${until?.let { "\"$it\"" } ?: "null"},"desiredHeld":$held,
                    "settled":true,"micReachable":true,"stateToken":"t"}""",
            )!!
        assertEquals(Window.Off, state(false, null, false).desiredWindow())
        assertEquals(Window.Hold(end), state(false, "2026-10-16T12:00:00Z", true).desiredWindow())
        // Recording with no end is recall's own fault, and records.
        assertNull(state(true, null, false).desiredWindow())
    }

    @Test
    fun malformedJsonIsNullNotACrash() {
        assertNull(parseCaptureState("not json"))
    }

    @Test
    fun anAnswerMissingAFieldIsNull() {
        // An older recall's answer: the phone shows nothing rather than guess.
        assertNull(
            parseCaptureState(
                """{"running":true,"pausedUntil":null,"desiredRunning":true,
                    "desiredPausedUntil":null,"settled":true,"micReachable":true,
                    "stateToken":"t"}""",
            ),
        )
    }
}
