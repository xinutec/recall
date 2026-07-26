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
 * Posts the "recording resumes soon" heads-up when [ResumeWarning]'s alarm fires. A
 * plain informational notification: tapping it opens the app on the pause controls, so
 * the pause can be extended before the mic comes back on.
 */
class ResumeWarningReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        val millis = intent.getLongExtra(ResumeWarning.EXTRA_RESUME_AT_MILLIS, 0L)
        // Without a resume time the warning has nothing to say — skip rather than post a
        // bare notification (defensive; the scheduler always sets the extra).
        if (millis <= 0L) return
        val resumeAt = Instant.ofEpochMilli(millis)

        val mgr = context.getSystemService(NotificationManager::class.java)
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            mgr.createNotificationChannel(
                NotificationChannel(
                    CHANNEL_ID,
                    "Resume warning",
                    // LOW: silent — no sound, no vibration, no heads-up peek. It just
                    // appears in the shade / status bar, so the room mic never makes a
                    // noise. (Channel importance is locked at first creation, so this
                    // must ship before any warning ever fires.)
                    NotificationManager.IMPORTANCE_LOW,
                ),
            )
        }
        mgr.notify(NOTIFICATION_ID, build(context, resumeAt))
    }

    private fun build(context: Context, resumeAt: Instant) =
        NotificationCompat
            .Builder(context, CHANNEL_ID)
            .setContentTitle("Recording resumes soon")
            .setContentText(resumeWarningText(resumeAt, Instant.now(), ZoneId.systemDefault()))
            .setSmallIcon(R.drawable.ic_mic)
            .setColor(ContextCompat.getColor(context, R.color.ic_launcher_background))
            // Silent on pre-O too (the compat mirror of the LOW channel): no sound/peek.
            .setPriority(NotificationCompat.PRIORITY_LOW)
            .setAutoCancel(true)
            .setContentIntent(launchApp(context))
            .build()

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

        // Distinct from the service's ongoing notification (id 1) so the two coexist.
        const val NOTIFICATION_ID = 2
    }
}
