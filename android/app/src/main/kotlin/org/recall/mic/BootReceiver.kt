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
 * After a reboot, restarts streaming if it was enabled. Where a boot-started mic service
 * cannot record ([BootPolicy]), it posts a prompt instead; a tap opens MainActivity,
 * which restarts streaming.
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
                // If refused anyway, prompt.
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
        mgr.notify(NotificationIds.BOOT, notification)
    }

    companion object {
        private const val CHANNEL_ID = "boot-resume"

        /**
         * Take down the reboot prompt once streaming is back. `setAutoCancel` only
         * covers a tap, and streaming usually returns by opening the app.
         */
        fun clearResumePrompt(context: Context) {
            context
                .getSystemService(NotificationManager::class.java)
                .cancel(NotificationIds.BOOT)
        }
    }
}
