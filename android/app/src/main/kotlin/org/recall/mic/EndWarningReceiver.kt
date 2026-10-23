package org.recall.mic

import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import androidx.core.app.NotificationCompat
import androidx.core.content.ContextCompat
import java.time.Instant
import java.time.ZoneId

/**
 * Posts "recording stops soon" when [EndWarning]'s alarm fires; tapping it opens the
 * app's recording controls. Moving the window's end takes it down ([EndWarning]).
 */
class EndWarningReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        val millis = intent.getLongExtra(EndWarning.EXTRA_END_AT_MILLIS, 0L)
        if (millis <= 0L) return // the scheduler always sets it
        val endAt = Instant.ofEpochMilli(millis)

        val mgr = context.getSystemService(NotificationManager::class.java)
        mgr.createNotificationChannel(
            NotificationChannel(
                CHANNEL_ID,
                "Recording ending",
                // Silent: no sound, vibration or peek, so a room phone never makes a
                // noise. Fixed once the channel exists.
                NotificationManager.IMPORTANCE_LOW,
            ),
        )
        mgr.notify(NotificationIds.END_WARNING, build(context, endAt))
    }

    private fun build(context: Context, endAt: Instant) =
        NotificationCompat
            .Builder(context, CHANNEL_ID)
            .setContentTitle("Recording stops soon")
            .setContentText(endWarningText(endAt, Instant.now(), ZoneId.systemDefault()))
            .setSmallIcon(R.drawable.ic_mic)
            .setColor(ContextCompat.getColor(context, R.color.ic_launcher_background))
            .setAutoCancel(true)
            // Gone at the end even if the app never polls again; at once if the alarm
            // came late.
            .setTimeoutAfter(msUntil(endAt).coerceAtLeast(1))
            .setContentIntent(launchApp(context))
            .build()

    private fun msUntil(endAt: Instant) = endAt.toEpochMilli() - System.currentTimeMillis()

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
        const val CHANNEL_ID = "end-warning"
    }
}
