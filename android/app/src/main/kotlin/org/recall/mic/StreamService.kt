package org.recall.mic

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.media.AudioFormat
import android.media.AudioRecord
import android.media.MediaRecorder
import android.os.Build
import android.os.IBinder
import android.os.PowerManager
import android.util.Log
import androidx.core.app.NotificationCompat
import androidx.core.content.ContextCompat
import kotlinx.coroutines.runBlocking
import java.io.OutputStream
import java.net.InetSocketAddress
import java.net.Socket
import java.time.Instant
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean
import kotlin.concurrent.thread

/**
 * Foreground service (type `microphone`, the supported way to hold the mic open
 * indefinitely) that streams the mic as raw 48 kHz mono s16le PCM over TCP to the
 * recorder host, reconnecting on any drop. The host's ingest takes this PCM as it
 * is, so the phone neither resamples nor encodes. Alongside, [SegmentWriter] keeps
 * FLAC copies that [SegmentUpload] delivers.
 */
class StreamService : Service() {
    @Volatile private var running = false

    // Kept so Stop can close it: a write blocked on a dead network then throws at
    // once instead of holding the mic and wakelock for TCP's ~15 min timeout.
    @Volatile private var activeSocket: Socket? = null
    private var worker: Thread? = null
    private var beater: Thread? = null
    private var wakeLock: PowerManager.WakeLock? = null
    private var lastNotificationText: String? = null

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        if (running) return START_STICKY
        val host = Prefs.host(this)
        // Isis, asked whether a failed connect is a pause (see Prefs).
        val controlHost = Prefs.controlHost(this)
        val deviceId = Prefs.deviceId(this)

        if (!startInForeground()) {
            // Refused, as for a start from the background; running on would stream
            // silence.
            stopSelf()
            return START_NOT_STICKY
        }
        running = true
        MicState.setRunning(true)
        // Streaming again, however it was started: the reboot prompt is moot.
        BootReceiver.clearResumePrompt(this)
        worker = thread(name = "mic-stream") { streamLoop(host, controlHost, deviceId) }
        beater = thread(name = "mic-heartbeat") { beatLoop(controlHost, host, deviceId) }
        return START_STICKY
    }

    override fun onDestroy() {
        running = false
        // Close unblocks a stalled write; interrupt, a sleep.
        runCatching { activeSocket?.close() }
        worker?.interrupt()
        beater?.interrupt()
        worker?.join(JOIN_TIMEOUT_MS)
        beater?.join(JOIN_TIMEOUT_MS)
        MicState.setRunning(false)
        MicState.setConnected(false)
        releaseWakeLock()
        super.onDestroy()
    }

    /** Connect and capture until stopped, reconnecting after a delay. `host` takes the
     * PCM; `controlHost` (Isis) only tells a pause from an unreachable host. */
    private fun streamLoop(host: String, controlHost: String, deviceId: String) {
        val minBuf = AudioRecord.getMinBufferSize(SAMPLE_RATE, CHANNEL, ENCODING)
        // ~1s of headroom so a brief network stall doesn't drop mic frames.
        val bufSize = maxOf(minBuf, SAMPLE_RATE * BYTES_PER_SAMPLE)
        while (running) {
            var record: AudioRecord? = null
            var socket: Socket? = null
            // The local FLAC segments. Per cycle: a segment's name claims its audio
            // is continuous from its stamp, so none may span a reconnect.
            var segments: SegmentWriter? = null
            try {
                // Connect first, open the mic only on success. The host is a home
                // LAN address, so away from home the mic never opens. (Android 14+
                // hides the SSID from a background service.)
                socket =
                    Socket().apply {
                        tcpNoDelay = true
                        // Detects a peer that vanished without closing.
                        keepAlive = true
                        connect(InetSocketAddress(host, INGEST_PORT), CONNECT_TIMEOUT_MS)
                    }
                activeSocket = socket
                // Only while streaming, so a paused or away phone can sleep between
                // attempts.
                acquireWakeLock()
                setNotification("Streaming to $host")
                MicState.setConnected(true)
                // Recording is on: no resume warning is needed.
                ResumeWarning.cancel(this)
                // Connected means home, which is what queued meetings wait for.
                MeetingUpload.enqueue(this)
                // The household pause state comes from /api/capture, never from
                // whether this socket connected.
                record = openRecord(bufSize)
                record.startRecording()
                // So a meeting recording waits for the release (#1472).
                MicHandover.acquired()
                MicState.setMicOk(true)
                segments =
                    getExternalFilesDir(android.os.Environment.DIRECTORY_MUSIC)?.let { dir ->
                        SegmentWriter(
                            dir,
                            deviceId,
                            onFailure = { Log.w(TAG, it) },
                            onSegmentClosed = { SegmentUpload.enqueue(this) },
                        ).also { it.start() }
                    }
                val out: OutputStream = socket.getOutputStream()
                // One line naming the device on the shared port, then PCM. The epoch
                // is the capture time of the first samples, which the server stamps
                // the segments with (docs/devices.md).
                out.write(
                    handshakeLine(
                        deviceId,
                        SAMPLE_RATE,
                        epochMillis = System.currentTimeMillis(),
                    ).toByteArray(),
                )
                // The mic loop never writes the socket: a blocked write would stop
                // read() and overrun AudioRecord's buffer. It fills the spool, and a
                // sender thread drains it.
                val spool = PcmSpool(SPOOL_BYTES)
                // An exception escaping a bare thread kills the app, so the sender
                // only flags failure, and the mic loop reconnects.
                val senderFailed = AtomicBoolean(false)
                val sender =
                    thread(name = "mic-sender") {
                        try {
                            while (running) {
                                val pending = spool.drain()
                                if (pending.isEmpty()) {
                                    Thread.sleep(SENDER_IDLE_MS)
                                } else {
                                    out.write(pending)
                                }
                            }
                        } catch (e: Exception) {
                            Log.w(TAG, "sender: stream write failed: ${e.message}")
                            senderFailed.set(true)
                        }
                    }
                val chunk = ByteArray(READ_CHUNK_BYTES)
                var counted = 0L
                try {
                    while (running) {
                        if (senderFailed.get()) {
                            error("sender thread lost the connection — reconnecting")
                        }
                        val n = record.read(chunk, 0, chunk.size)
                        if (n > 0) {
                            spool.offer(chunk, n)
                            val dropped = spool.dropped()
                            if (dropped > counted) {
                                MicState.addDroppedBytes(dropped - counted)
                                counted = dropped
                            }
                            segments?.offer(chunk, n)
                            MicState.setLevel(peakLevel(chunk, n))
                        } else if (n < 0) {
                            throw IllegalStateException("AudioRecord.read returned $n")
                        }
                    }
                } finally {
                    sender.join(SENDER_JOIN_MS)
                }
            } catch (e: Exception) {
                // Stopped: leave without re-posting the notification.
                if (!running) break
                MicState.setMicOk(micOkAfter(MicState.micOk.value, e))
                // dev-lint: allow-detekt one prelude for both outcomes; two catches would repeat the stop check
                if (e is MicUnavailableException) {
                    // The connect succeeded; the mic did not.
                    setNotification("Microphone unavailable — check permission / other apps")
                    Log.w(TAG, "mic init failed: ${e.message}")
                } else {
                    // A pause also closes the host's listener, so ask the API, and
                    // publish it for the screen and the notification alike.
                    // While paused the loop waits here, so this is where onDestroy's
                    // interrupt lands when a meeting takes the mic; uncaught, it
                    // would kill the app.
                    val cap =
                        try {
                            runBlocking { CaptureApi.state(controlHost) }
                        } catch (_: InterruptedException) {
                            break
                        }
                    MicState.setCapture(cap)
                    // Polled every few seconds while paused: where the resume warning
                    // is kept in step with the pause.
                    ResumeWarning.sync(this, cap, Instant.now())
                    val paused = cap?.let { !it.running } == true
                    setNotification(
                        if (paused) "Recording paused" else "Waiting for recall host",
                    )
                    Log.w(TAG, "not streaming (host unreachable / dropped): ${e.message}")
                }
            } finally {
                activeSocket = null
                MicState.setConnected(false)
                releaseWakeLock()
                runCatching { record?.stop() }
                runCatching { record?.release() }
                // After the release, never before.
                MicHandover.released()
                runCatching { socket?.close() }
                runCatching { segments?.close() }
                if (segments != null) SegmentUpload.enqueue(this)
            }
            // No wakelock: the CPU may sleep between attempts (longer in doze).
            if (running) {
                try {
                    Thread.sleep(RECONNECT_DELAY_MS)
                } catch (_: InterruptedException) {
                    break // onDestroy
                }
            }
        }
    }

    /**
     * The hourly heartbeat, for as long as the service lives (#837), starting at once.
     * Its own thread: [streamLoop] blocks in `read` or a reconnect sleep, and a beat
     * there would keep the schedule of the very thing it reports on.
     */
    private fun beatLoop(controlHost: String, host: String, deviceId: String) {
        // Consecutive failures, which shorten the next wait (#886).
        var failures = 0
        val drops = Heartbeat.DropsSinceBeat()
        while (running) {
            val dropped = MicState.droppedBytes.value
            val landed =
                Heartbeat.send(
                    controlHost,
                    host,
                    deviceId,
                    MicState.connected.value,
                    MicState.micOk.value,
                    drops.pending(dropped),
                    this,
                )
            if (landed) drops.landed(dropped)
            failures = if (landed) 0 else failures + 1
            try {
                Thread.sleep(TimeUnit.MINUTES.toMillis(Heartbeat.nextDelayMinutes(failures)))
            } catch (_: InterruptedException) {
                break // onDestroy
            }
        }
    }

    private fun setNotification(text: String) {
        if (text == lastNotificationText) return
        lastNotificationText = text
        getSystemService(NotificationManager::class.java)
            .notify(NotificationIds.STREAM, buildNotification(text))
    }

    /**
     * UNPROCESSED where supported (no automatic gain or noise suppression, which
     * harm speaker identification), else MIC.
     */
    private fun openRecord(bufSize: Int): AudioRecord {
        val sources =
            intArrayOf(
                MediaRecorder.AudioSource.UNPROCESSED,
                MediaRecorder.AudioSource.MIC,
            )
        for (source in sources) {
            val record = AudioRecord(source, SAMPLE_RATE, CHANNEL, ENCODING, bufSize)
            if (record.state == AudioRecord.STATE_INITIALIZED) return record
            record.release()
        }
        throw MicUnavailableException(
            "could not initialise AudioRecord (permission revoked / mic held elsewhere?)",
        )
    }

    /**
     * Enter the foreground with the microphone type; false if refused, as Android 15
     * does for a start from the background such as BOOT_COMPLETED.
     */
    private fun startInForeground(): Boolean {
        val mgr = getSystemService(NotificationManager::class.java)
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            mgr.createNotificationChannel(
                NotificationChannel(
                    CHANNEL_ID,
                    "Mic stream",
                    NotificationManager.IMPORTANCE_LOW,
                ),
            )
        }
        lastNotificationText = "Starting…"
        val notification = buildNotification(lastNotificationText!!)
        return runCatching {
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
                startForeground(
                    NotificationIds.STREAM,
                    notification,
                    ServiceInfo.FOREGROUND_SERVICE_TYPE_MICROPHONE,
                )
            } else {
                startForeground(NotificationIds.STREAM, notification)
            }
        }.onFailure { Log.w(TAG, "foreground start refused: ${it.message}") }.isSuccess
    }

    private fun buildNotification(text: String): Notification {
        // Tapping it opens the app.
        val launch =
            Intent(this, MainActivity::class.java).apply {
                flags = Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_CLEAR_TOP
            }
        val pending =
            PendingIntent.getActivity(
                this,
                0,
                launch,
                PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
            )
        return NotificationCompat
            .Builder(this, CHANNEL_ID)
            .setContentTitle("Recall Mic")
            .setContentText(text)
            .setSmallIcon(R.drawable.ic_mic)
            .setColor(ContextCompat.getColor(this, R.color.ic_launcher_background))
            .setOngoing(true)
            .setContentIntent(pending)
            .build()
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
        private const val TAG = "StreamService"
        private const val CHANNEL_ID = "mic-stream"
        private const val WAKE_TAG = "recall-mic:stream"

        // What the host's ingest expects: 48 kHz, mono, s16le.
        private const val SAMPLE_RATE = 48000
        private const val CHANNEL = AudioFormat.CHANNEL_IN_MONO
        private const val ENCODING = AudioFormat.ENCODING_PCM_16BIT
        private const val BYTES_PER_SAMPLE = 2

        // ~43 ms per read, so the level meter is lively.
        private const val READ_CHUNK_BYTES = 4096

        // 60 s of PCM (~5.8 MB): rides out a busy host or a Wi-Fi stall.
        private const val SPOOL_BYTES = SAMPLE_RATE * BYTES_PER_SAMPLE * 60
        private const val SENDER_IDLE_MS = 20L
        private const val SENDER_JOIN_MS = 2000L

        private const val CONNECT_TIMEOUT_MS = 5000
        private const val RECONNECT_DELAY_MS = 2000L
        private const val JOIN_TIMEOUT_MS = 2000L

        // The ingest port all devices share (`audiod ingest`).
        private const val INGEST_PORT = 9999

        fun start(ctx: Context) {
            val intent = Intent(ctx, StreamService::class.java)
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
                ctx.startForegroundService(intent)
            } else {
                ctx.startService(intent)
            }
        }

        fun stop(ctx: Context) {
            ctx.stopService(Intent(ctx, StreamService::class.java))
        }
    }
}
