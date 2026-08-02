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
 * Foreground service that records one meeting to a file — the appointment/meeting mode,
 * as opposed to [StreamService]'s continuous household capture.
 *
 * **Ogg/Opus, and the container is the crash strategy.** A truncated Ogg still decodes to
 * its last complete page, so a recording cut short by a flat battery or a kill costs its
 * tail rather than the whole appointment. An m4a interrupted before `stop()` has no `moov`
 * atom and is not recoverable at all — which is why this is one file per meeting instead
 * of rolled parts plus a join. (Android has no MP3 encoder; Opus is the format it encodes
 * well, and `.ogg` is already accepted by the server's upload endpoint.)
 *
 * The audio source is `UNPROCESSED` falling back to `MIC` — the same preference
 * [StreamService] makes, for the same reason: automatic gain control and noise suppression
 * damage the speaker embeddings the diarizer depends on. The bitrate is above continuous
 * capture's 32 kbps because a meeting is far-field with several voices and a one-off hour
 * costs ~25 MB, so the storage argument doesn't apply.
 *
 * Recording is deliberate, so it wins the microphone: starting stops the stream, and
 * stopping starts it again if it was enabled. Both modes are in one process precisely so
 * that rule can be enforced — across two apps the loser would only find out via a failed
 * recorder init.
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
        val title = intent?.getStringExtra(EXTRA_TITLE).orEmpty()
        when (intent?.action) {
            ACTION_STOP -> finish(title)
            else -> begin(title)
        }
        return START_NOT_STICKY // a killed recording is finished, never silently restarted
    }

    override fun onDestroy() {
        // The system killed us mid-recording (or the process is going away): close the
        // file properly rather than leaving the last page unwritten.
        if (recorder != null) finish(null)
        super.onDestroy()
    }

    private fun begin(title: String) {
        if (recorder != null) return // already recording; a second Start is a no-op
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.Q) {
            // Opus encoding (and the OGG container) arrived in Android 10. Nothing below
            // that can produce the format, and silently writing a different one would put
            // an unrecoverable m4a where the crash strategy expects an Ogg. minSdk stays
            // 26 because an older phone can still do the streaming job.
            MeetingState.setError("Meeting recording needs Android 10 or newer.")
            stopSelf()
            return
        }
        beginOnQ(title)
    }

    @RequiresApi(Build.VERSION_CODES.Q)
    private fun beginOnQ(title: String) {
        if (!startInForeground()) {
            // The OS refused a mic-type foreground start — recording on would capture
            // nothing but silence.
            MeetingState.setError("Android refused to start recording in the foreground.")
            stopSelf()
            return
        }

        // The deliberate act wins the microphone. Prefs.enabled is untouched, so it is
        // also the record of whether to put the stream back afterwards — a field would
        // not survive this service being killed.
        StreamService.stop(this)

        val start = Instant.now()
        val file =
            File(MeetingQueue.dir(this), MeetingQueue.fileName(start, ZoneId.systemDefault()))
        // Sidecar first: from here on, a crash leaves a file that still knows what it is.
        MeetingQueue.writeSidecar(file, title, start)

        val started =
            openRecorder(file, MediaRecorder.AudioSource.UNPROCESSED)
                ?: openRecorder(file, MediaRecorder.AudioSource.MIC)
        if (started == null) {
            MeetingQueue.discard(file)
            MeetingState.setError("Microphone unavailable — check permission / other apps.")
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
                @Suppress("DEPRECATION")
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

    /**
     * Finish the recording and hand it to the upload queue. [title] is what the user had
     * typed when they pressed Stop — null when the system is tearing us down, in which
     * case whatever was written at the start stands.
     */
    private fun finish(title: String?) {
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

        // stop() throws when the recorder never got a valid frame (Stop pressed
        // instantly, or the mic died) — that leaves an empty file, not a short one.
        val kept =
            synchronized(recorderLock) {
                runCatching { rec.stop() }.isSuccess.also { runCatching { rec.release() } }
            }
        releaseWakeLock()
        MeetingState.setRecording(false)

        if (file != null) {
            if (kept && file.length() > 0) {
                title?.let { MeetingQueue.writeSidecar(file, it, startOf(file)) }
                Log.i(UI_LOG, "meeting saved: ${file.name} (${file.length()} bytes)")
                // Saved, and that is all. It stays on the phone to be listened to; only
                // an explicit Upload hands it to MeetingUpload.
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

    /** The start already recorded for [file] — the sidecar's, else its filename's. */
    private fun startOf(file: File): Instant =
        MeetingQueue.readSidecar(file)?.second
            ?: MeetingQueue.startFromName(file.name, ZoneId.systemDefault())
            ?: Instant.now()

    /** Put continuous capture back if it was running before this recording took the mic. */
    private fun restoreStream() {
        if (Prefs.enabled(this) && Prefs.host(this).isNotBlank()) StreamService.start(this)
    }

    /**
     * `MediaRecorder` never hands over the samples, so the level meter polls the peak
     * amplitude it accumulated since the last read — the same 0..32767 units
     * [amplitudeLevel] scales for the streaming meter, so the two read alike.
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
                NOTIFICATION_ID,
                buildNotification("Starting…", null),
                ServiceInfo.FOREGROUND_SERVICE_TYPE_MICROPHONE,
            )
        }.onFailure { Log.w(TAG, "foreground start refused: ${it.message}") }.isSuccess
    }

    private fun setNotification(text: String, since: Instant?) {
        getSystemService(NotificationManager::class.java)
            .notify(NOTIFICATION_ID, buildNotification(text, since))
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
                // Stopping from the shade keeps the title written at the start — the
                // alternative is walking out of an appointment with the recorder still on.
                .addAction(R.drawable.ic_mic, "Stop", stop)
        // A live elapsed counter for free, so the shade shows how long it has been going.
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
        private const val NOTIFICATION_ID = 2 // 1 is the stream's
        private const val WAKE_TAG = "recall-mic:meeting"

        private const val ACTION_STOP = "org.recall.mic.STOP_MEETING"
        private const val EXTRA_TITLE = "title"

        // 48 kHz mono, as the rest of recall. 56 kbps sits in the 48-64 range a far-field
        // multi-voice meeting wants — above continuous capture's 32 kbps.
        private const val SAMPLE_RATE = 48000
        private const val BITRATE = 56000

        private const val METER_INTERVAL_MS = 100L

        fun start(ctx: Context, title: String) {
            ctx.startForegroundService(
                Intent(ctx, MeetingService::class.java).putExtra(EXTRA_TITLE, title),
            )
        }

        fun stop(ctx: Context, title: String) {
            ctx.startService(
                Intent(ctx, MeetingService::class.java)
                    .setAction(ACTION_STOP)
                    .putExtra(EXTRA_TITLE, title),
            )
        }
    }
}
