package org.recall.mic

import android.app.AlarmManager
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.util.Log
import java.time.Instant

/**
 * Acts on [planResumeWarning]: schedules (or cancels) a single exact alarm that fires
 * [RESUME_WARNING_LEAD] before the household recording auto-resumes, so a
 * [ResumeWarningReceiver] can post a heads-up in time to extend the pause.
 *
 * Driven off the same capture-state reads the pause banner uses — the foreground
 * [StreamService] poll (the reliable background path, since a paused mic can't connect
 * so the service polls the control host every few seconds) and the open screen's
 * long-poll — so pressing "Still away (24h)" moves the alarm the moment the intent lands.
 *
 * An exact, allow-while-idle alarm is used (not the service's own poll loop) so the
 * warning keeps its full lead time even in doze, and independent of the poll cadence.
 *
 * It also takes a posted heads-up back down once the pause moves out from under it. That
 * needs nothing new: the same state reads already re-arm the alarm, and because the plan
 * is derived from the household state rather than from a button press, a pause extended
 * on the web or from the CLI clears the notification exactly like one extended here.
 */
object ResumeWarning {
    private const val TAG = "ResumeWarning"
    const val EXTRA_RESUME_AT_MILLIS = "resume_at_millis"

    // A distinct request code / action so this alarm's PendingIntent is stable: the
    // same intent is reused to reschedule (replacing the prior alarm) and to cancel.
    private const val REQUEST_CODE = 1001
    private const val ACTION = "org.recall.mic.RESUME_WARNING"

    // Last wall-clock time we scheduled for, so a state poll that re-derives the same
    // plan every couple of seconds doesn't re-arm an identical alarm each time. Process
    // state (same process as MicState); a fresh process just re-derives from the poll.
    @Volatile private var scheduledFor: Instant? = null

    /** Re-derive the plan from [capture] and schedule/cancel/leave the alarm to match. */
    fun sync(context: Context, capture: CaptureState?, now: Instant) {
        when (val plan = planResumeWarning(capture, now)) {
            is ResumeWarningPlan.Warn -> schedule(context, plan.at, plan.resumeAt)
            is ResumeWarningPlan.Cancel -> cancel(context)
            is ResumeWarningPlan.Leave -> Unit
        }
    }

    private fun schedule(context: Context, at: Instant, resumeAt: Instant) {
        if (at == scheduledFor) return // already armed for this exact moment
        // The warn moment moved, so any heads-up already in the shade is stale by
        // construction: it is only ever posted when the alarm fires, and a Warn plan
        // means the warn moment is still ahead of us. Take it down, so extending the
        // pause clears the warning no matter where it was extended from — this screen,
        // the web UI, or the CLI — instead of leaving a wrong resume time on display.
        dismiss(context)
        val alarms = context.getSystemService(AlarmManager::class.java)
        val pending = pendingIntent(context, resumeAt)
        try {
            alarms.setExactAndAllowWhileIdle(AlarmManager.RTC_WAKEUP, at.toEpochMilli(), pending)
            scheduledFor = at
            Log.i(UI_LOG, "resume-warning armed for $at (recording resumes $resumeAt)")
        } catch (e: SecurityException) {
            // Exact alarms not permitted (should not happen with USE_EXACT_ALARM, but
            // never crash the capture service over a reminder): fall back to inexact.
            alarms.setAndAllowWhileIdle(AlarmManager.RTC_WAKEUP, at.toEpochMilli(), pending)
            scheduledFor = at
            Log.w(TAG, "exact alarm denied, scheduled inexact: ${e.message}")
        }
    }

    /** Drop any pending warning (recording is back on, or was never within reach).
     *  Always hits AlarmManager — after a process restart `scheduledFor` is null yet a
     *  real alarm may still be pending, and cancelling a non-existent alarm is a no-op. */
    fun cancel(context: Context) {
        context
            .getSystemService(AlarmManager::class.java)
            .cancel(pendingIntent(context, resumeAt = null))
        // The alarm may already have fired, leaving the heads-up in the shade: there is
        // nothing left to warn about, so it goes too.
        dismiss(context)
        if (scheduledFor != null) {
            scheduledFor = null
            Log.i(UI_LOG, "resume-warning cancelled")
        }
    }

    /** Take down a posted warning. Cancelling a notification that was never posted is a
     *  no-op, so this is safe on every plan change — including the common case where the
     *  alarm is still pending and nothing is on screen. */
    private fun dismiss(context: Context) {
        context
            .getSystemService(NotificationManager::class.java)
            .cancel(NotificationIds.RESUME_WARNING)
    }

    // FLAG_UPDATE_CURRENT refreshes the resume-at extra when rescheduling; the request
    // code + action are constant so this addresses the one warning alarm for cancel too.
    private fun pendingIntent(context: Context, resumeAt: Instant?): PendingIntent {
        val intent =
            Intent(context, ResumeWarningReceiver::class.java).apply {
                action = ACTION
                resumeAt?.let { putExtra(EXTRA_RESUME_AT_MILLIS, it.toEpochMilli()) }
            }
        return PendingIntent.getBroadcast(
            context,
            REQUEST_CODE,
            intent,
            PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
        )
    }
}
