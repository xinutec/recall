package org.recall.mic

import java.time.Duration
import java.time.Instant
import java.time.ZoneId
import java.time.format.DateTimeFormatter

/*
 * Whether and when to warn that a recording window is about to end, so a long call
 * can be extended in time. Pure, so unit-tested; EndWarning acts on it.
 */

/** How long before the window's end the warning comes. */
val END_WARNING_LEAD: Duration = Duration.ofMinutes(10)

private val WARNING_TIME = DateTimeFormatter.ofPattern("HH:mm")

/** What to do with the pending "recording stops soon" warning. */
sealed interface EndWarningPlan {
    /** Warn at [at]; recording stops at [endAt]. */
    data class Warn(
        val at: Instant,
        val endAt: Instant,
    ) : EndWarningPlan

    /** Nothing to warn about. */
    data object Cancel : EndWarningPlan

    /** Keep what is scheduled: the state is unknown, or the warning is due or past. */
    data object Leave : EndWarningPlan
}

/**
 * The plan for a capture state, from its desired half, so moving the end moves the
 * warning without waiting for the mic. Only a recording window warns: off, a paused
 * window and a fault have no end that stops anything.
 */
fun planEndWarning(
    capture: CaptureState?,
    now: Instant,
    lead: Duration = END_WARNING_LEAD,
): EndWarningPlan {
    // A failed poll.
    if (capture == null) return EndWarningPlan.Leave
    val window = capture.desiredWindow() as? Window.Record ?: return EndWarningPlan.Cancel
    val endAt = window.end
    if (!endAt.isAfter(now)) return EndWarningPlan.Cancel
    val warnAt = endAt.minus(lead)
    // Inside the lead: a short window, or the alarm fired or is about to.
    if (!warnAt.isAfter(now)) return EndWarningPlan.Leave
    return EndWarningPlan.Warn(warnAt, endAt)
}

/** The warning's text: "Recording stops in 10m (at 11:00) — tap to extend". */
fun endWarningText(endAt: Instant, now: Instant, zone: ZoneId): String {
    val at = endAt.atZone(zone).format(WARNING_TIME)
    return "Recording stops in ${Banner.remaining(endAt, now)} (at $at) — tap to extend"
}
