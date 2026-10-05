package org.recall.mic

import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.os.Build
import androidx.core.app.NotificationCompat
import androidx.core.content.ContextCompat
import java.time.Instant
import java.time.ZoneId

/**
 * Posts "recording resumes soon" when [ResumeWarning]'s alarm fires; tapping it opens
 * the app's pause controls. Extending the pause takes it down ([ResumeWarning]).
 */
class ResumeWarningReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        val millis = intent.getLongExtra(ResumeWarning.EXTRA_RESUME_AT_MILLIS, 0L)
        if (millis <= 0L) return // the scheduler always sets it
        val resumeAt = Instant.ofEpochMilli(millis)

        val mgr = context.getSystemService(NotificationManager::class.java)
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            mgr.createNotificationChannel(
                NotificationChannel(
                    CHANNEL_ID,
                    "Resume warning",
                    // Silent: no sound, vibration or peek, so a room phone never makes
                    // a noise. Fixed once the channel exists.
                    NotificationManager.IMPORTANCE_LOW,
                ),
            )
        }
        mgr.notify(NotificationIds.RESUME_WARNING, build(context, resumeAt))
    }

    private fun build(context: Context, resumeAt: Instant) =
        NotificationCompat
            .Builder(context, CHANNEL_ID)
            .setContentTitle("Recording resumes soon")
            .setContentText(resumeWarningText(resumeAt, Instant.now(), ZoneId.systemDefault()))
            .setSmallIcon(R.drawable.ic_mic)
            .setColor(ContextCompat.getColor(context, R.color.ic_launcher_background))
            // Silent before Android 8 too.
            .setPriority(NotificationCompat.PRIORITY_LOW)
            .setAutoCancel(true)
            // Gone at the resume even if the app never polls again; at once if the alarm
            // came late.
            .setTimeoutAfter(msUntil(resumeAt).coerceAtLeast(1))
            .setContentIntent(launchApp(context))
            .build()

    private fun msUntil(resumeAt: Instant) = resumeAt.toEpochMilli() - System.currentTimeMillis()

    private fun launchApp(context: Context): PendingIntent {
        val launch =
            Intent(context, MainActivity::class.java).apply {
                flags = Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_CLEAR_TOP
            }
        return PendingIntent.getActivity(
            context,
            0,
            launch,
            PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
        )
    }

    private companion object {
        const val CHANNEL_ID = "resume-warning"
    }
}
