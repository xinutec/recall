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
 * Foreground service that captures the microphone as raw 48 kHz mono s16le PCM and
 * streams it over a plain TCP socket to the recall host, reconnecting on any drop.
 *
 * This is the supported way to hold the mic open indefinitely on Android — a
 * foreground service with the `microphone` type. The phone is the TCP *client*:
 * the recall host listens and ffmpeg ingests the raw PCM exactly as it does the
 * USB mic's sox stream. The byte format matches recall's CaptureConfig (48000 Hz,
 * 1 channel, 16-bit signed little-endian), so the phone neither resamples nor
 * re-encodes — it just ships PCM.
 */
class StreamService : Service() {
    @Volatile private var running = false

    // Kept so Stop can abort a write blocked on a dead network: closing the socket
    // from onDestroy makes the blocked write throw immediately. Without it the
    // worker thread can sit in `out.write` for the full TCP retransmission timeout
    // (~15 min), holding the mic and wakelock while the UI already says "Stopped".
    @Volatile private var activeSocket: Socket? = null
    private var worker: Thread? = null
    private var beater: Thread? = null
    private var wakeLock: PowerManager.WakeLock? = null
    private var lastNotificationText: String? = null

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        if (running) return START_STICKY
        val host = Prefs.host(this)
        // The capture API moved to Isis (the control host), separate from the recorder
        // host the stream connects to — so the pause-vs-unreachable check below asks Isis,
        // not the Mac's (retired) API. See Prefs / CaptureApi.
        val controlHost = Prefs.controlHost(this)
        val deviceId = Prefs.deviceId(this)

        if (!startInForeground()) {
            // The OS refused the mic-type foreground start (e.g. launched from the
            // background on modern Android). Running on anyway would stream silence.
            stopSelf()
            return START_NOT_STICKY
        }
        running = true
        MicState.setRunning(true)
        // Streaming is back, however it was asked for — a tap on the reboot prompt, the
        // app being opened, or the boot auto-start. Either way the prompt has nothing
        // left to ask for.
        BootReceiver.clearResumePrompt(this)
        worker = thread(name = "mic-stream") { streamLoop(host, controlHost, deviceId) }
        beater = thread(name = "mic-heartbeat") { beatLoop(controlHost, host, deviceId) }
        return START_STICKY
    }

    override fun onDestroy() {
        running = false
        // Unblock the worker wherever it is: a stalled write (close) or a
        // reconnect/pause sleep (interrupt) — then reap it.
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

    /** Capture/connect until stopped, reconnecting after any failure with a delay.
     * `host` is the recorder host the PCM stream connects to; `controlHost` is Isis, asked
     * only to tell a deliberate pause from an unreachable recorder. */
    private fun streamLoop(host: String, controlHost: String, deviceId: String) {
        val minBuf = AudioRecord.getMinBufferSize(SAMPLE_RATE, CHANNEL, ENCODING)
        // ~1s of headroom so a brief network stall doesn't drop mic frames.
        val bufSize = maxOf(minBuf, SAMPLE_RATE * BYTES_PER_SAMPLE)
        while (running) {
            var record: AudioRecord? = null
            var socket: Socket? = null
            // Store-and-forward shadow (docs/architecture.md, stage C1): beside
            // the live stream, the same PCM lands in closed, capture-stamped
            // local segments that SegmentUpload delivers with verified
            // receipts. Per CYCLE, not per service: a segment must never span
            // a reconnect gap — its name claims its audio is continuous from
            // the stamp, and the mic really was closed in between.
            var segments: SegmentWriter? = null
            try {
                // Connect FIRST, open the mic only on success. The recall host is a
                // private home-LAN address, reachable only when the phone is on the
                // home network — so off it (mobile data, another Wi-Fi) the connect
                // fails and the mic never opens: nothing is recorded unless it can
                // actually be delivered to recall. (This replaces SSID matching,
                // which Android 14+ won't expose to a background service.)
                socket =
                    Socket().apply {
                        tcpNoDelay = true
                        // Probe a silently-dead peer (AP power-cycle, host sleep)
                        // instead of writing into a black hole until TCP gives up.
                        keepAlive = true
                        connect(InetSocketAddress(host, INGEST_PORT), CONNECT_TIMEOUT_MS)
                    }
                activeSocket = socket
                // Hold the wakelock only while actually streaming, so a paused or
                // away phone (no server to connect to) lets the CPU deep-sleep
                // between reconnect attempts instead of being held awake.
                acquireWakeLock()
                setNotification("Streaming to $host")
                MicState.setConnected(true)
                // Recording is definitively on now — drop any pending resume warning
                // (a resume the app never polled as running would otherwise leave it armed).
                ResumeWarning.cancel(this)
                // Connecting proves we're on the home network, which is the one thing a
                // queued meeting recording is waiting for. Cheap, and it means walking
                // back in uploads yesterday's appointment without opening anything.
                MeetingUpload.enqueue(this)
                // Note: connecting drives only *this phone's* state. The household
                // pause state is the authority's (/api/capture + Pause/Resume) — never
                // inferred from a socket, which races a parking listener on pause.
                record = openRecord(bufSize)
                record.startRecording()
                // The mic opened: clear any earlier failure so a recovered app stops
                // reporting a fault it no longer has.
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
                // Announce who we are on the shared ingest port, then stream PCM. The
                // server reads exactly this line, registers us by id, and segments the
                // rest. (One port for all devices; identity is the handshake, not a
                // port.) The epoch is taken now — recording just started, so it is the
                // capture instant of the first samples the loop below will stream; the
                // server uses it to rename this connection's segments from arrival
                // time back to capture time (see docs/devices.md).
                out.write(
                    handshakeLine(
                        deviceId,
                        SAMPLE_RATE,
                        epochMillis = System.currentTimeMillis(),
                    ).toByteArray(),
                )
                // Read in small chunks (not the full ~1s buffer) so the UI level
                // meter is responsive.
                //
                // ⚠ The mic loop NEVER touches the socket. It used to write each
                // chunk straight out, so when the recorder host was busy TCP
                // backpressure blocked the write, read() was not called, and
                // AudioRecord's own ~1s buffer overran — the phone dropped speech
                // it had already heard. Measured 2026-09-03 with the Mac at load
                // 42: phone segment rotation slipped to 162s and 228s worst-case,
                // tracking the Mac's load. A recorder must not depend on its
                // consumer's mood, so capture now hands frames to a bounded spool
                // (PcmSpool) that never blocks, and a sender thread drains it.
                val spool = PcmSpool(SPOOL_BYTES)
                // The sender owns every socket write, and an exception escaping a
                // bare thread lambda KILLS THE APP (two data_app_crash records on
                // 2026-09-04, both "Software caused connection abort" at this
                // write). So nothing may escape: any failure here is flagged, and
                // the mic loop below turns the flag into the ordinary reconnect
                // path. The spool keeps absorbing frames during the handover.
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
                try {
                    while (running) {
                        if (senderFailed.get()) {
                            // Surface the sender's death HERE, where the catch
                            // below runs the normal teardown + reconnect + backoff.
                            error("sender thread lost the connection — reconnecting")
                        }
                        val n = record.read(chunk, 0, chunk.size)
                        if (n > 0) {
                            spool.offer(chunk, n)
                            segments?.offer(chunk, n)
                            MicState.setLevel(peakLevel(chunk, n))
                        } else if (n < 0) {
                            throw IllegalStateException("AudioRecord.read returned $n")
                        }
                    }
                } finally {
                    sender.join(SENDER_JOIN_MS)
                    if (spool.dropped() > 0) {
                        // The phone is the ONLY place that knows this happened, so
                        // it must say so rather than lose the audio silently.
                        MicState.setDroppedBytes(spool.dropped())
                    }
                }
            } catch (e: Exception) {
                // Service stopped: the closed socket / interrupt lands here — exit
                // without touching the notification (a zombie must not re-post one
                // after Stop) and without a pointless network call.
                if (!running) break
                MicState.setMicOk(e !is MicUnavailableException)
                if (e is MicUnavailableException) {
                    // Blaming the network would send whoever reads it debugging the
                    // wrong thing — the connect succeeded; the microphone didn't.
                    setNotification("Microphone unavailable — check permission / other apps")
                    Log.w(TAG, "mic init failed: ${e.message}")
                } else {
                    // Can't connect — but a deliberate pause closes the host's
                    // listener too. Ask the API (the single source of truth) and
                    // publish to the shared state, so the screen and notification
                    // render the one value and can't disagree about
                    // pause-vs-unreachable.
                    val cap = runBlocking { CaptureApi.state(controlHost) }
                    MicState.setCapture(cap)
                    // A paused mic can't connect, so this failure path polls the control
                    // host every couple of seconds while paused — the reliable place to
                    // (re)arm the 2h-before-resume warning as the pause is set or extended.
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
                runCatching { socket?.close() }
                // Close the cycle's segment (the mic is off; the file is
                // final) and offer the batch for delivery.
                runCatching { segments?.close() }
                if (segments != null) SegmentUpload.enqueue(this)
            }
            // No wakelock here: while disconnected the CPU may sleep between
            // attempts (the OS naturally stretches this during doze), so a paused
            // phone barely wakes; a connected phone that drops still retries fast.
            if (running) {
                try {
                    Thread.sleep(RECONNECT_DELAY_MS)
                } catch (_: InterruptedException) {
                    break // onDestroy interrupted the wait — exit cleanly
                }
            }
        }
    }

    /**
     * Say "still here" every hour for as long as this service lives (#837).
     *
     * Its own thread rather than a step in [streamLoop]: that loop blocks in
     * `record.read` for hours at a time while streaming, and sits in a reconnect sleep
     * while paused or away — so a beat folded into it would arrive on the schedule of
     * whatever the mic happened to be doing, which is the very thing being measured.
     * The thread costs one sleeping thread and one request an hour.
     *
     * Beats immediately on start, so a check that went red while the app was down
     * clears within a minute of it coming back rather than at the next hour mark.
     */
    private fun beatLoop(controlHost: String, host: String, deviceId: String) {
        // Consecutive failures, reset by any beat that lands. A blip must not cost an
        // hour of looking dead (#886): the sleep below is chosen from this, so the
        // first retry is a minute away rather than at the next hour mark.
        var failures = 0
        while (running) {
            val landed =
                Heartbeat.send(
                    controlHost,
                    host,
                    deviceId,
                    MicState.connected.value,
                    MicState.micOk.value,
                    this,
                )
            failures = if (landed) 0 else failures + 1
            try {
                Thread.sleep(TimeUnit.MINUTES.toMillis(Heartbeat.nextDelayMinutes(failures)))
            } catch (_: InterruptedException) {
                break // onDestroy interrupted the wait — exit cleanly
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
     * Prefer the UNPROCESSED source — the rawest signal, with no automatic gain
     * control or noise suppression, which is what the downstream speaker-ID and
     * separation want — and fall back to MIC where the device doesn't support it.
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

    /** Mic-init failure — distinct so the status can't blame the network for it. */
    private class MicUnavailableException(
        message: String,
    ) : Exception(message)

    /**
     * Enter the foreground with the microphone type. Returns false if the OS
     * refuses (a mic-type start from the background — e.g. BOOT_COMPLETED — throws
     * ForegroundServiceStartNotAllowedException on Android 15): the caller must
     * then stop rather than run a service that would only record silence.
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
        // Tapping the notification opens the app. Without a content intent the
        // notification is inert (can't be tapped to enter the app).
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
            // The app's own mic icon + the launcher's deep-blue accent (one colour
            // value), so the notification reads as Recall Mic too — not a generic
            // system mic with no tint.
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

        // Match recall's CaptureConfig: 48 kHz, mono, 16-bit signed little-endian.
        private const val SAMPLE_RATE = 48000
        private const val CHANNEL = AudioFormat.CHANNEL_IN_MONO
        private const val ENCODING = AudioFormat.ENCODING_PCM_16BIT
        private const val BYTES_PER_SAMPLE = 2

        // ~43ms of audio per read — small enough for a lively level meter.
        private const val READ_CHUNK_BYTES = 4096

        // Capture-to-network spool. 60s of 48kHz mono s16le (~5.8MB) — generous
        // enough to ride out a busy host or a Wi-Fi stall without the mic ever
        // pausing, small enough to be unremarkable on a phone.
        private const val SPOOL_BYTES = SAMPLE_RATE * BYTES_PER_SAMPLE * 60
        private const val SENDER_IDLE_MS = 20L
        private const val SENDER_JOIN_MS = 2000L

        private const val CONNECT_TIMEOUT_MS = 5000
        private const val RECONNECT_DELAY_MS = 2000L
        private const val JOIN_TIMEOUT_MS = 2000L

        // The one shared ingest port every device connects to (host-side `recall
        // ingest`); hardcoded, so nothing needs setting on the phone but the host.
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
