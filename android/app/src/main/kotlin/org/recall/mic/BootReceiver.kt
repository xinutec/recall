package org.recall.mic

import android.Manifest
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Build
import androidx.core.app.NotificationCompat
import androidx.core.content.ContextCompat

/**
 * Brings the mic back after a reboot, but only if the user had it enabled — so a
 * power blip or OS update doesn't silently leave the living-room mic dark.
 *
 * On modern Android a boot-started mic service can't actually record (see
 * BootPolicy), so there this posts a tap-to-resume notification instead of
 * starting a service that would stream silence; one tap opens MainActivity, whose
 * on-open resume path restarts streaming with full mic access.
 */
class BootReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        if (intent.action != Intent.ACTION_BOOT_COMPLETED) return
        val hasMic =
            ContextCompat.checkSelfPermission(context, Manifest.permission.RECORD_AUDIO) ==
                PackageManager.PERMISSION_GRANTED
        val action =
            BootPolicy.decide(
                sdkInt = Build.VERSION.SDK_INT,
                enabled = Prefs.enabled(context),
                hostSet = Prefs.host(context).isNotEmpty(),
                hasMicPermission = hasMic,
            )
        when (action) {
            BootAction.AUTO_START -> {
                // Belt and braces: if the OS still refuses, fall back to the prompt
                // rather than crashing the receiver.
                runCatching { StreamService.start(context) }
                    .onFailure { promptToResume(context) }
            }

            BootAction.PROMPT -> {
                promptToResume(context)
            }

            BootAction.NOTHING -> {
                Unit
            }
        }
    }

    private fun promptToResume(context: Context) {
        val mgr = context.getSystemService(NotificationManager::class.java)
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            mgr.createNotificationChannel(
                NotificationChannel(
                    CHANNEL_ID,
                    "Resume after reboot",
                    NotificationManager.IMPORTANCE_HIGH,
                ),
            )
        }
        val launch =
            Intent(context, MainActivity::class.java).apply {
                flags = Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_CLEAR_TOP
            }
        val pending =
            PendingIntent.getActivity(
                context,
                0,
                launch,
                PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
            )
        val notification =
            NotificationCompat
                .Builder(context, CHANNEL_ID)
                .setContentTitle("Recall Mic stopped by reboot")
                .setContentText("Tap to resume streaming")
                .setSmallIcon(R.drawable.ic_mic)
                .setColor(ContextCompat.getColor(context, R.color.ic_launcher_background))
                .setContentIntent(pending)
                .setAutoCancel(true)
                .build()
        mgr.notify(NOTIFICATION_ID, notification)
    }

    private companion object {
        const val CHANNEL_ID = "boot-resume"
        const val NOTIFICATION_ID = 2
    }
}
