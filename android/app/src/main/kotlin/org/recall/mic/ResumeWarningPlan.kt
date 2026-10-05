package org.recall.mic

import java.time.Duration
import java.time.Instant
import java.time.OffsetDateTime
import java.time.ZoneId
import java.time.format.DateTimeFormatter
import kotlin.math.max
import kotlin.math.roundToLong

/*
 * Whether and when to warn that recording is about to resume, so the pause can be
 * extended in time. Pure, so unit-tested; ResumeWarning acts on it.
 */

/** How long before the resume the warning comes. */
val RESUME_WARNING_LEAD: Duration = Duration.ofHours(2)

private val WARNING_DATE_TIME = DateTimeFormatter.ofPattern("yyyy-MM-dd HH:mm")

/** What to do with the pending "recording resumes soon" warning. */
sealed interface ResumeWarningPlan {
    /** Warn at [at]; recording resumes at [resumeAt]. */
    data class Warn(
        val at: Instant,
        val resumeAt: Instant,
    ) : ResumeWarningPlan

    /** Nothing to warn about. */
    data object Cancel : ResumeWarningPlan

    /** Keep what is scheduled: the state is unknown, or the warning is due or past. */
    data object Leave : ResumeWarningPlan
}

/**
 * The plan for the household capture state, from its desired half, so extending the
 * pause moves the warning without waiting for the mic.
 */
fun planResumeWarning(
    capture: CaptureState?,
    now: Instant,
    lead: Duration = RESUME_WARNING_LEAD,
): ResumeWarningPlan {
    // A failed poll.
    if (capture == null) return ResumeWarningPlan.Leave
    if (capture.desiredRunning) return ResumeWarningPlan.Cancel
    val resumeIso = capture.desiredPausedUntil ?: capture.pausedUntil
    val resumeAt =
        resumeIso?.let { runCatching { OffsetDateTime.parse(it).toInstant() }.getOrNull() }
            ?: return ResumeWarningPlan.Cancel
    if (!resumeAt.isAfter(now)) return ResumeWarningPlan.Cancel
    val warnAt = resumeAt.minus(lead)
    // Inside the lead: a short pause, or the alarm fired or is about to.
    if (!warnAt.isAfter(now)) return ResumeWarningPlan.Leave
    return ResumeWarningPlan.Warn(warnAt, resumeAt)
}

/**
 * The warning's text, phrased like the [Banner] countdown: "Recording auto-resumes in
 * 2h 0m (by 2026-07-04 08:30) — tap to extend the pause".
 */
fun resumeWarningText(resumeAt: Instant, now: Instant, zone: ZoneId): String {
    val by = OffsetDateTime.ofInstant(resumeAt, zone).format(WARNING_DATE_TIME)
    return "Recording auto-resumes in ${remaining(resumeAt, now)} (by $by) — " +
        "tap to extend the pause"
}

/** Time left as "2h 0m", "23m" or "now": whole minutes, never negative. Unlike the
 *  Banner's, keeps "0m" at a whole hour. */
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
