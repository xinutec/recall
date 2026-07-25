package org.recall.mic

import java.time.Duration
import java.time.Instant
import java.time.OffsetDateTime
import java.time.ZoneId
import java.time.format.DateTimeFormatter
import kotlin.math.max
import kotlin.math.roundToLong

/**
 * Decides — purely, from a [CaptureState] snapshot — whether to warn that the
 * household recording is about to auto-resume, so there's time to extend the pause
 * before the mic comes back on. All the timing logic lives here (no Android in the
 * loop), so it's unit-tested exactly like [Banner]; the AlarmManager plumbing that
 * acts on the decision is the thin [ResumeWarning] object.
 */

/** How far ahead of the auto-resume we warn. Long enough to notice and re-pause. */
val RESUME_WARNING_LEAD: Duration = Duration.ofHours(2)

private val WARNING_DATE_TIME = DateTimeFormatter.ofPattern("yyyy-MM-dd HH:mm")

/** What to do with the pending "recording resumes soon" warning. */
sealed interface ResumeWarningPlan {
    /**
     * Schedule (or move) the warning to wall-clock instant [at]; [resumeAt] is when
     * recording actually resumes, carried through for the notification text.
     */
    data class Warn(val at: Instant, val resumeAt: Instant) : ResumeWarningPlan

    /** Recording is on (or resuming): drop any pending warning. */
    data object Cancel : ResumeWarningPlan

    /**
     * Leave whatever is already scheduled untouched — either the state is unknown (a
     * failed poll) or we're already inside the lead window, where the alarm is about
     * to fire (or has fired) and must not be cancelled out from under itself.
     */
    data object Leave : ResumeWarningPlan
}

/**
 * Decide the warning from the household capture state. Keyed on the DESIRED view (the
 * intent that moves the instant a button is pressed), so extending the pause
 * reschedules the warning at once, without waiting for the mic to confirm.
 */
fun planResumeWarning(
    capture: CaptureState?,
    now: Instant,
    lead: Duration = RESUME_WARNING_LEAD,
): ResumeWarningPlan {
    // A failed poll (null) must not cancel a good alarm — leave it be.
    if (capture == null) return ResumeWarningPlan.Leave
    // Running or resuming: there is no pending pause to warn about.
    if (capture.desiredRunning) return ResumeWarningPlan.Cancel
    val resumeIso = capture.desiredPausedUntil ?: capture.pausedUntil
    val resumeAt =
        resumeIso?.let { runCatching { OffsetDateTime.parse(it).toInstant() }.getOrNull() }
            ?: return ResumeWarningPlan.Cancel
    // The pause has already elapsed (about to auto-resume): nothing to warn ahead of.
    if (!resumeAt.isAfter(now)) return ResumeWarningPlan.Cancel
    val warnAt = resumeAt.minus(lead)
    // Inside the lead window already — either a pause shorter than the lead (no
    // ahead-of-time moment exists) or the alarm has already fired. Leave, so a sync
    // crossing the boundary can never cancel the alarm microseconds before it fires.
    if (!warnAt.isAfter(now)) return ResumeWarningPlan.Leave
    return ResumeWarningPlan.Warn(warnAt, resumeAt)
}

/**
 * The warning notification's body, e.g.
 * "Recording auto-resumes in 2h 0m (by 2026-07-04 08:30) — tap to extend the pause".
 * Phrased like the [Banner] countdown so the two read as one system.
 */
fun resumeWarningText(resumeAt: Instant, now: Instant, zone: ZoneId): String {
    val by = OffsetDateTime.ofInstant(resumeAt, zone).format(WARNING_DATE_TIME)
    return "Recording auto-resumes in ${remaining(resumeAt, now)} (by $by) — " +
        "tap to extend the pause"
}

/** Time left as "2h 0m" / "23m" / "now" — whole minutes, never negative. Matches the
 *  Banner countdown's shape (but keeps the minutes even at a whole hour, so the lead
 *  reads "2h 0m" rather than a bare "2h"). */
private fun remaining(until: Instant, now: Instant): String {
    val mins =
        max(
            0L,
            (until.toEpochMilli() - now.toEpochMilli()).toDouble().div(60_000).roundToLong(),
        )
    if (mins == 0L) return "now"
    val h = mins / 60
    val m = mins % 60
    return if (h > 0) "${h}h ${m}m" else "${m}m"
}
