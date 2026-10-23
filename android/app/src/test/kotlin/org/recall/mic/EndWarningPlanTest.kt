package org.recall.mic

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import java.time.Duration
import java.time.Instant
import java.time.ZoneId

class EndWarningPlanTest {
    private val london = ZoneId.of("Europe/London")
    private val lead = Duration.ofMinutes(10)
    private val now = Instant.parse("2026-07-04T08:00:00Z")

    /** A settled state whose desired window is [window]. */
    private fun state(window: Window?) =
        CaptureState(
            running = window is Window.Record,
            until = window?.end?.toString(),
            held = window is Window.Hold,
            desiredRunning = window == null || window is Window.Record,
            desiredUntil = window?.end?.toString(),
            desiredHeld = window is Window.Hold,
            settled = true,
            micReachable = true,
            stateToken = "t",
        )

    @Test
    fun warnsTenMinutesBeforeTheWindowEnds() {
        val end = Instant.parse("2026-07-04T09:00:00Z")
        val warn = planEndWarning(state(Window.Record(end)), now, lead) as EndWarningPlan.Warn
        assertEquals(Instant.parse("2026-07-04T08:50:00Z"), warn.at)
        assertEquals(end, warn.endAt)
    }

    @Test
    fun keysOnTheDesiredEndSoAnExtendReschedulesAtOnce() {
        // The mic still reports the old end; the warning follows the press.
        val extending =
            state(Window.Record(Instant.parse("2026-07-04T08:30:00Z")))
                .copy(desiredUntil = "2026-07-04T10:00:00Z", settled = false)
        val warn = planEndWarning(extending, now, lead) as EndWarningPlan.Warn
        assertEquals(Instant.parse("2026-07-04T09:50:00Z"), warn.at)
    }

    @Test
    fun cancelsWhenNothingRecordsToAnEnd() {
        val end = Instant.parse("2026-07-04T09:00:00Z")
        for (window in listOf(Window.Off, Window.Hold(end), null)) {
            assertEquals("$window", EndWarningPlan.Cancel, planEndWarning(state(window), now, lead))
        }
    }

    @Test
    fun leavesTheAlarmAloneOnAFailedPoll() {
        assertEquals(EndWarningPlan.Leave, planEndWarning(null, now, lead))
    }

    @Test
    fun leavesInsteadOfCancellingOnceInsideTheLead() {
        // Not a cancel: a sync crossing the warn moment must not kill the alarm the
        // instant before it fires.
        val end = Instant.parse("2026-07-04T08:05:00Z")
        assertEquals(EndWarningPlan.Leave, planEndWarning(state(Window.Record(end)), now, lead))
    }

    @Test
    fun cancelsOnceTheWindowHasEnded() {
        val end = Instant.parse("2026-07-04T07:59:00Z")
        assertEquals(EndWarningPlan.Cancel, planEndWarning(state(Window.Record(end)), now, lead))
    }

    @Test
    fun warningTextNamesTheEndInLocalTime() {
        val end = Instant.parse("2026-07-04T09:00:00Z") // 10:00 BST
        assertEquals(
            "Recording stops in 10m (at 10:00) — tap to extend",
            endWarningText(end, Instant.parse("2026-07-04T08:50:00Z"), london),
        )
    }

    @Test
    fun warningTextNeverGoesNegative() {
        val end = Instant.parse("2026-07-04T07:30:00Z")
        assertTrue(endWarningText(end, now, london).contains("in now (at"))
    }
}
