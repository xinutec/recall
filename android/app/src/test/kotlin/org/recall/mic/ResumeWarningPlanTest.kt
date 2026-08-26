package org.recall.mic

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import java.time.Duration
import java.time.Instant
import java.time.ZoneId

class ResumeWarningPlanTest {
    private val london = ZoneId.of("Europe/London")
    private val lead = Duration.ofHours(2)

    /** A paused snapshot with the given desired resume-by (the intent the UI follows). */
    private fun paused(desiredUntil: String?, confirmedUntil: String? = desiredUntil) =
        CaptureState(
            running = false,
            pausedUntil = confirmedUntil,
            desiredRunning = false,
            desiredPausedUntil = desiredUntil,
            settled = true,
            micReachable = true,
        )

    @Test
    fun warnsTwoHoursBeforeALongPauseResumes() {
        // Paused until 08:30Z; now 06:00Z → warn at 06:30Z (2h before), resume 08:30Z.
        val plan =
            planResumeWarning(
                paused("2026-07-04T08:30:00+00:00"),
                Instant.parse("2026-07-04T06:00:00Z"),
                lead,
            )
        val warn = plan as ResumeWarningPlan.Warn
        assertEquals(Instant.parse("2026-07-04T06:30:00Z"), warn.at)
        assertEquals(Instant.parse("2026-07-04T08:30:00Z"), warn.resumeAt)
    }

    @Test
    fun keysOnTheDesiredResumeSoAnExtendReschedulesAtOnce() {
        // Mid-extend: the mic still reports the old resume-by, but the intent already
        // moved to the later one — the warning must follow the intent, not the lag.
        val extending =
            CaptureState(
                running = false,
                pausedUntil = "2026-07-04T08:30:00+00:00", // old, not yet caught up
                desiredRunning = false,
                desiredPausedUntil = "2026-07-05T08:30:00+00:00", // just pressed +24h
                settled = false,
                micReachable = true,
            )
        val warn =
            planResumeWarning(extending, Instant.parse("2026-07-04T06:00:00Z"), lead)
                as ResumeWarningPlan.Warn
        assertEquals(Instant.parse("2026-07-05T06:30:00Z"), warn.at)
    }

    @Test
    fun cancelsWhenRunning() {
        val running =
            CaptureState(
                running = true,
                pausedUntil = null,
                desiredRunning = true,
                desiredPausedUntil = null,
                settled = true,
                micReachable = true,
            )
        assertEquals(
            ResumeWarningPlan.Cancel,
            planResumeWarning(running, Instant.parse("2026-07-04T06:00:00Z"), lead),
        )
    }

    @Test
    fun cancelsWhileResumingEvenBeforeItSettles() {
        // "Resume now" pressed: desiredRunning flips true before the mic confirms — no
        // pending pause to warn about, so drop the alarm immediately.
        val resuming =
            CaptureState(
                running = false,
                pausedUntil = "2026-07-04T08:30:00+00:00",
                desiredRunning = true,
                desiredPausedUntil = null,
                settled = false,
                micReachable = true,
            )
        assertEquals(
            ResumeWarningPlan.Cancel,
            planResumeWarning(resuming, Instant.parse("2026-07-04T06:00:00Z"), lead),
        )
    }

    @Test
    fun leavesTheAlarmAloneOnAFailedPoll() {
        // A null (unreachable) snapshot must not cancel a good, already-armed alarm.
        assertEquals(
            ResumeWarningPlan.Leave,
            planResumeWarning(null, Instant.parse("2026-07-04T06:00:00Z"), lead),
        )
    }

    @Test
    fun leavesInsteadOfCancellingOnceInsideTheLeadWindow() {
        // now 07:00Z, resume 08:30Z → only 1h30m left (< 2h lead). No ahead-of-time
        // moment, and crucially NOT a cancel — so a sync crossing the warn boundary
        // can't kill the alarm the instant before it fires.
        assertEquals(
            ResumeWarningPlan.Leave,
            planResumeWarning(
                paused("2026-07-04T08:30:00+00:00"),
                Instant.parse("2026-07-04T07:00:00Z"),
                lead,
            ),
        )
    }

    @Test
    fun extendingFromInsideTheLeadWindowWarnsAgainRatherThanLeaving() {
        // The heads-up has already fired (resume 08:30Z, now 07:00Z, inside the 2h lead)
        // and the pause is then extended by a day — from this screen, the web UI or the
        // CLI, the plan cannot tell and must not care. Going back to Warn is what re-arms
        // the alarm AND takes down the notification still on screen, which would
        // otherwise keep showing yesterday's resume time.
        val warn =
            planResumeWarning(
                paused("2026-07-05T08:30:00+00:00"),
                Instant.parse("2026-07-04T07:00:00Z"),
                lead,
            ) as ResumeWarningPlan.Warn
        assertEquals(Instant.parse("2026-07-05T06:30:00Z"), warn.at)
        assertEquals(Instant.parse("2026-07-05T08:30:00Z"), warn.resumeAt)
    }

    @Test
    fun cancelsWhenThePauseHasElapsed() {
        // Resume time already passed (about to auto-resume): nothing to warn ahead of.
        assertEquals(
            ResumeWarningPlan.Cancel,
            planResumeWarning(
                paused("2026-07-04T08:30:00+00:00"),
                Instant.parse("2026-07-04T09:00:00Z"),
                lead,
            ),
        )
    }

    @Test
    fun cancelsWhenPausedButNoResumeTimeIsKnown() {
        // Paused with neither a desired nor confirmed resume-by (old server): can't
        // compute a warn moment, so hold no alarm.
        assertEquals(
            ResumeWarningPlan.Cancel,
            planResumeWarning(paused(null), Instant.parse("2026-07-04T06:00:00Z"), lead),
        )
    }

    @Test
    fun fallsBackToConfirmedResumeWhenDesiredIsAbsent() {
        // Old server sends only the confirmed view; desiredPausedUntil mirrors it via
        // the parser, but guard the null-desired path directly too.
        val onlyConfirmed =
            CaptureState(
                running = false,
                pausedUntil = "2026-07-04T08:30:00+00:00",
                desiredRunning = false,
                desiredPausedUntil = null,
                settled = true,
                micReachable = true,
            )
        val warn =
            planResumeWarning(onlyConfirmed, Instant.parse("2026-07-04T06:00:00Z"), lead)
                as ResumeWarningPlan.Warn
        assertEquals(Instant.parse("2026-07-04T06:30:00Z"), warn.at)
    }

    @Test
    fun ignoresAnUnparseableResumeTime() {
        assertEquals(
            ResumeWarningPlan.Cancel,
            planResumeWarning(paused("not-a-date"), Instant.parse("2026-07-04T06:00:00Z"), lead),
        )
    }

    @Test
    fun warningTextReadsLikeTheBannerCountdown() {
        // 2h ahead of an 08:30 BST resume, London zone.
        val resumeAt = Instant.parse("2026-07-04T07:30:00Z") // 08:30 BST
        val now = Instant.parse("2026-07-04T05:30:00Z") // 06:30 BST
        assertEquals(
            "Recording auto-resumes in 2h 0m (by 2026-07-04 08:30) — tap to extend the pause",
            resumeWarningText(resumeAt, now, london),
        )
    }

    @Test
    fun warningTextNeverGoesNegative() {
        val resumeAt = Instant.parse("2026-07-04T07:30:00Z")
        val now = Instant.parse("2026-07-04T09:00:00Z") // past the resume
        assertTrue(resumeWarningText(resumeAt, now, london).contains("in now (by"))
    }
}
