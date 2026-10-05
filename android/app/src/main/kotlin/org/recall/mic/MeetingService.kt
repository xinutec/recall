package org.recall.mic

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.media.MediaRecorder
import android.os.Build
import android.os.IBinder
import android.os.PowerManager
import android.util.Log
import androidx.annotation.RequiresApi
import androidx.core.app.NotificationCompat
import androidx.core.content.ContextCompat
import java.io.File
import java.time.Instant
import java.time.ZoneId
import kotlin.concurrent.thread

/**
 * Foreground service that records one meeting to a file, as opposed to [StreamService]'s
 * continuous capture.
 *
 * Ogg/Opus, because a truncated Ogg still decodes to its last page, so a flat battery or
 * a kill costs only the tail; an m4a cut short before `stop()` has no `moov` atom and is
 * lost. Hence one file per meeting.
 *
 * `UNPROCESSED`, falling back to `MIC`, as in [StreamService].
 *
 * A meeting wins the microphone: starting stops the stream, and stopping restarts it if
 * it was enabled. Both are in one process so that rule can be enforced.
 */
class MeetingService : Service() {
    private val recorderLock = Any()
    private var recorder: MediaRecorder? = null
    private var audio: File? = null
    private var startedAt: Instant? = null
    private var meter: Thread? = null
    private var wakeLock: PowerManager.WakeLock? = null

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        when (intent?.action) {
            ACTION_STOP -> finish()
            else -> begin()
        }
        return START_NOT_STICKY // a killed recording is finished, not restarted
    }

    override fun onDestroy() {
        // Torn down mid-recording: close the file so the last page is written.
        if (recorder != null) finish()
        super.onDestroy()
    }

    private fun begin() {
        if (recorder != null) return
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.Q) {
            // Ogg/Opus needs Android 10; minSdk stays 26 for streaming.
            MeetingState.setError("Meeting recording needs Android 10 or newer.")
            stopSelf()
            return
        }
        beginOnQ()
    }

    @RequiresApi(Build.VERSION_CODES.Q)
    private fun beginOnQ() {
        if (!startInForeground()) {
            MeetingState.setError("Android refused to start recording in the foreground.")
            stopSelf()
            return
        }

        // Prefs.enabled is left alone: it says whether to restart the stream after,
        // and survives this service being killed.
        StreamService.stop(this)
        // stopService is asynchronous, so wait for the capture thread to release the
        // mic (#1472); bounded, and on a timeout try anyway (see MicHandover).
        val handedOver = MicHandover.awaitRelease(MicHandover.HANDOVER_MS)

        val start = Instant.now()
        val file =
            File(MeetingQueue.dir(this), MeetingQueue.fileName(start, ZoneId.systemDefault()))

        val started =
            openRecorder(file, MediaRecorder.AudioSource.UNPROCESSED)
                ?: openRecorder(file, MediaRecorder.AudioSource.MIC)
        if (started == null) {
            MeetingQueue.discard(file)
            MeetingState.setError(MicHandover.failureMessage(handedOver))
            restoreStream()
            stopSelf()
            return
        }

        recorder = started
        audio = file
        startedAt = start
        acquireWakeLock()
        MeetingState.setError(null)
        MeetingState.setRecording(true, start, file)
        setNotification("Recording — tap to open", start)
        meter = thread(name = "meeting-meter") { meterLoop() }
        Log.i(UI_LOG, "meeting recording to ${file.name}")
    }

    /** Configure and start a [MediaRecorder] on [source], or null if it won't run. */
    @RequiresApi(Build.VERSION_CODES.Q)
    private fun openRecorder(file: File, source: Int): MediaRecorder? {
        val rec =
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
                MediaRecorder(this)
            } else {
                @Suppress("DEPRECATION") // the Context-less constructor is all there is below S
                MediaRecorder()
            }
        return runCatching {
            rec.setAudioSource(source)
            rec.setOutputFormat(MediaRecorder.OutputFormat.OGG)
            rec.setAudioEncoder(MediaRecorder.AudioEncoder.OPUS)
            rec.setAudioSamplingRate(SAMPLE_RATE)
            rec.setAudioChannels(1)
            rec.setAudioEncodingBitRate(BITRATE)
            rec.setOutputFile(file)
            rec.prepare()
            rec.start()
            rec
        }.getOrElse {
            Log.w(TAG, "recorder on source $source failed: ${it.message}")
            runCatching { rec.release() }
            null
        }
    }

    /** Close the file and list it, on Stop or on teardown. */
    private fun finish() {
        val rec =
            recorder ?: run {
                stopSelf()
                return
            }
        val file = audio
        recorder = null
        audio = null
        startedAt = null
        meter?.interrupt()
        meter = null

        // stop() throws when no frame was recorded; the file is then empty.
        val kept =
            synchronized(recorderLock) {
                runCatching { rec.stop() }.isSuccess.also { runCatching { rec.release() } }
            }
        releaseWakeLock()
        MeetingState.setRecording(false)

        if (file != null) {
            if (kept && file.length() > 0) {
                // Kept on the phone until the user uploads it.
                Log.i(UI_LOG, "meeting saved: ${file.name} (${file.length()} bytes)")
            } else {
                Log.w(UI_LOG, "meeting discarded: ${file.name} — no audio was written")
                MeetingQueue.discard(file)
                MeetingState.setError("Nothing was recorded — the file was empty.")
            }
        }
        MeetingLibrary.refresh(this)
        restoreStream()
        stopSelf()
    }

    /** Restart continuous capture if it is enabled. */
    private fun restoreStream() {
        if (Prefs.enabled(this) && Prefs.host(this).isNotBlank()) StreamService.start(this)
    }

    /**
     * `MediaRecorder` does not hand over samples, so the meter polls its peak amplitude
     * since the last read, in the 0..32767 units [amplitudeLevel] scales.
     */
    private fun meterLoop() {
        while (!Thread.currentThread().isInterrupted) {
            val peak =
                synchronized(recorderLock) {
                    recorder?.let { runCatching { it.maxAmplitude }.getOrDefault(0) } ?: return
                }
            MeetingState.setLevel(amplitudeLevel(peak))
            try {
                Thread.sleep(METER_INTERVAL_MS)
            } catch (_: InterruptedException) {
                return
            }
        }
    }

    @RequiresApi(Build.VERSION_CODES.Q)
    private fun startInForeground(): Boolean {
        val mgr = getSystemService(NotificationManager::class.java)
        mgr.createNotificationChannel(
            NotificationChannel(
                CHANNEL_ID,
                "Meeting recording",
                NotificationManager.IMPORTANCE_LOW,
            ),
        )
        return runCatching {
            startForeground(
                NotificationIds.MEETING,
                buildNotification("Starting…", null),
                ServiceInfo.FOREGROUND_SERVICE_TYPE_MICROPHONE,
            )
        }.onFailure { Log.w(TAG, "foreground start refused: ${it.message}") }.isSuccess
    }

    private fun setNotification(text: String, since: Instant?) {
        getSystemService(NotificationManager::class.java)
            .notify(NotificationIds.MEETING, buildNotification(text, since))
    }

    private fun buildNotification(text: String, since: Instant?): Notification {
        val open =
            PendingIntent.getActivity(
                this,
                0,
                Intent(this, MeetingActivity::class.java).apply {
                    flags = Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_CLEAR_TOP
                },
                PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
            )
        val stop =
            PendingIntent.getService(
                this,
                1,
                Intent(this, MeetingService::class.java).setAction(ACTION_STOP),
                PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
            )
        val builder =
            NotificationCompat
                .Builder(this, CHANNEL_ID)
                .setContentTitle("Recall — recording meeting")
                .setContentText(text)
                .setSmallIcon(R.drawable.ic_mic)
                .setColor(ContextCompat.getColor(this, R.color.ic_launcher_background))
                .setOngoing(true)
                .setContentIntent(open)
                .addAction(R.drawable.ic_mic, "Stop", stop)
        // An elapsed-time counter in the shade.
        since?.let {
            builder.setUsesChronometer(true).setWhen(it.toEpochMilli()).setShowWhen(true)
        }
        return builder.build()
    }

    private fun acquireWakeLock() {
        val wl =
            wakeLock ?: (getSystemService(Context.POWER_SERVICE) as PowerManager)
                .newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, WAKE_TAG)
                .also {
                    it.setReferenceCounted(false)
                    wakeLock = it
                }
        if (!wl.isHeld) wl.acquire()
    }

    private fun releaseWakeLock() {
        runCatching { if (wakeLock?.isHeld == true) wakeLock?.release() }
    }

    companion object {
        private const val TAG = "MeetingService"
        private const val CHANNEL_ID = "meeting-record"
        private const val WAKE_TAG = "recall-mic:meeting"

        private const val ACTION_STOP = "org.recall.mic.STOP_MEETING"

        // 48 kHz mono, as the rest of recall; 56 kbps Opus suits a far-field meeting
        // with several voices (~25 MB an hour).
        private const val SAMPLE_RATE = 48000
        private const val BITRATE = 56000

        private const val METER_INTERVAL_MS = 100L

        fun start(ctx: Context) {
            ctx.startForegroundService(Intent(ctx, MeetingService::class.java))
        }

        fun stop(ctx: Context) {
            ctx.startService(Intent(ctx, MeetingService::class.java).setAction(ACTION_STOP))
        }
    }
}
