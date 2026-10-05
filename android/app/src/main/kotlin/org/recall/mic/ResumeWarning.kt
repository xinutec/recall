package org.recall.mic

import android.app.AlarmManager
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.util.Log
import java.time.Instant

/**
 * Acts on [planResumeWarning]: one exact alarm, [RESUME_WARNING_LEAD] before recording
 * resumes, for [ResumeWarningReceiver] to post a heads-up in time to extend the pause.
 * Exact and allowed while idle, so doze cannot shorten the lead.
 *
 * Synced from the capture-state reads the pause banner uses ([StreamService]'s poll,
 * the open screen's long poll). Since the plan follows the household state, a pause
 * extended anywhere (here, the web, the CLI) moves the alarm and clears a stale warning.
 */
object ResumeWarning {
    private const val TAG = "ResumeWarning"
    const val EXTRA_RESUME_AT_MILLIS = "resume_at_millis"

    // Fixed, so one PendingIntent both reschedules and cancels the alarm.
    private const val REQUEST_CODE = 1001
    private const val ACTION = "org.recall.mic.RESUME_WARNING"

    // When the alarm is armed for, so each poll does not re-arm the same one.
    // dev-lint: allow-object-var a memo of the last armed alarm; a fresh process re-derives it
    @Volatile private var scheduledFor: Instant? = null

    /** Arm, cancel or leave the alarm to match [capture]. */
    fun sync(context: Context, capture: CaptureState?, now: Instant) {
        when (val plan = planResumeWarning(capture, now)) {
            is ResumeWarningPlan.Warn -> schedule(context, plan.at, plan.resumeAt)
            is ResumeWarningPlan.Cancel -> cancel(context)
            is ResumeWarningPlan.Leave -> Unit
        }
    }

    private fun schedule(context: Context, at: Instant, resumeAt: Instant) {
        if (at == scheduledFor) return
        // The warn moment moved, so a posted warning shows a wrong time.
        dismiss(context)
        val alarms = context.getSystemService(AlarmManager::class.java)
        val pending = pendingIntent(context, resumeAt)
        try {
            alarms.setExactAndAllowWhileIdle(AlarmManager.RTC_WAKEUP, at.toEpochMilli(), pending)
            scheduledFor = at
            Log.i(UI_LOG, "resume-warning armed for $at (recording resumes $resumeAt)")
        } catch (e: SecurityException) {
            // Not expected with USE_EXACT_ALARM; inexact rather than crash the service.
            alarms.setAndAllowWhileIdle(AlarmManager.RTC_WAKEUP, at.toEpochMilli(), pending)
            scheduledFor = at
            Log.w(TAG, "exact alarm denied, scheduled inexact: ${e.message}")
        }
    }

    /** Drop the alarm and any posted warning. Always asks AlarmManager: after a restart
     *  `scheduledFor` is null while an alarm may still be pending. */
    fun cancel(context: Context) {
        context
            .getSystemService(AlarmManager::class.java)
            .cancel(pendingIntent(context, resumeAt = null))
        dismiss(context)
        if (scheduledFor != null) {
            scheduledFor = null
            Log.i(UI_LOG, "resume-warning cancelled")
        }
    }

    /** Take down a posted warning; a no-op if none is. */
    private fun dismiss(context: Context) {
        context
            .getSystemService(NotificationManager::class.java)
            .cancel(NotificationIds.RESUME_WARNING)
    }

    // FLAG_UPDATE_CURRENT refreshes the resume-at extra on rescheduling.
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
