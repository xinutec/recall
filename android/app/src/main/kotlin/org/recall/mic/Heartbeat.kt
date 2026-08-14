package org.recall.mic

import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.os.BatteryManager
import org.json.JSONObject
import java.net.HttpURLConnection
import java.net.URL
import java.time.Instant
import java.time.format.DateTimeFormatter

/**
 * "I am still here", once an hour, whether or not there is anything to stream.
 *
 * recall could not tell a dead recorder from a quiet room. Its per-source liveness marker
 * is refreshed only by audio above the silence floor — deliberately, so that a dot means
 * *recording* rather than merely connected — and while the household is paused the ingest
 * listener is closed and nothing streams at all. Capture was paused for the four days
 * before this was written, which is exactly the window in which a dead app goes unnoticed
 * until somebody picks the phone up (#837).
 *
 * Sent to the CONTROL host (Isis, over WireGuard), not the recorder on the LAN — the same
 * split [CaptureApi] already makes. Isis is reachable from anywhere, so a phone that is
 * out of the house still beats and "away" stops looking like "dead". That is also why
 * this is not merged into [OutboxReport]: that one rides the upload worker's schedule and
 * only exists on phones that record meetings; this must beat from the streaming service,
 * on every mic phone, on its own clock.
 *
 * ⚠ Beats only while the service is running, which is what "started" means here. A
 * stopped app is not going to record, and a beat that arrived anyway would paint the one
 * state worth catching — this mic will capture nothing — bright green.
 *
 * Best-effort and silent, like [OutboxReport]: a liveness report that raised its own
 * failures would be the tail wagging the dog.
 */
object Heartbeat {
    private const val API_PORT = 8000 // `recall api --port 8000`, as CaptureApi
    private const val TIMEOUT_MS = 8000

    /**
     * How often to beat. The grader's thresholds are written as multiples of this
     * (`recall.mic_alive.BEAT_EVERY_MINUTES`), so the two cannot drift apart silently.
     */
    const val EVERY_MINUTES = 60L

    /**
     * First retry after a beat that did not land. Doubles per consecutive failure up to
     * [EVERY_MINUTES], so a blip costs minutes rather than an hour (#886).
     */
    private const val RETRY_BASE_MINUTES = 1L

    /**
     * When this process started. Set on class load — the service is the first thing to
     * touch it — so a beat carrying a recent value means the app restarted, and "up all
     * week" is distinguishable from "relaunching between beats".
     */
    val startedAt: Instant = Instant.now()

    /**
     * Minutes until the next beat: the full cadence when the last one landed, a short
     * backoff when it did not.
     *
     * Pure, so the schedule is pinned in unit tests without a network or an hour of
     * waiting — the same reason [body] is pure.
     *
     * ⚠ The cap is what keeps this a BACKOFF and not a poll. One request an hour is the
     * design; a phone that is simply off (or out of range for a week) must never beat
     * harder than that, and the whole retry burst is bounded to fit inside one cadence.
     * Growing it would trade the thing this exists to protect for a little less latency.
     */
    fun nextDelayMinutes(consecutiveFailures: Int): Long {
        if (consecutiveFailures <= 0) return EVERY_MINUTES
        // Shift-free: `1L shl 63` would wrap, and this counter is reset only by a
        // success, so a phone left in a dead spot keeps incrementing it forever.
        var delay = RETRY_BASE_MINUTES
        repeat(consecutiveFailures - 1) {
            if (delay >= EVERY_MINUTES) return EVERY_MINUTES
            delay *= 2
        }
        return minOf(delay, EVERY_MINUTES)
    }

    /** App version and build, so a restart *into a new build* reads as a deploy. */
    fun version(ctx: Context): String =
        runCatching {
            val info = ctx.packageManager.getPackageInfo(ctx.packageName, 0)
            @Suppress("DEPRECATION")
            "${info.versionName} (${info.versionCode})"
        }.getOrDefault("?")

    /**
     * The JSON a beat carries — pure, so the field names (a contract with the server's
     * `HeartbeatIn`) can be pinned in a unit test without a network.
     */
    fun body(
        device: String,
        version: String,
        startedAt: Instant,
        streaming: Boolean,
        charging: Boolean?,
        micOk: Boolean,
    ): String =
        JSONObject()
            .put("device", device)
            .put("app", "android")
            .put("version", version)
            .put("startedAt", DateTimeFormatter.ISO_INSTANT.format(startedAt))
            .put("streaming", streaming)
            // A running app that cannot open its mic must SAY so rather than fall
            // silent, which is what it used to do (#887).
            .put("micOk", micOk)
            // Absent rather than null when unknown: guessing "discharging" would invent
            // the very reading a mains-powered room phone is watched for.
            .apply { if (charging != null) put("charging", charging) }
            .toString()

    /**
     * True on mains, false on battery, null when Android will not say. Room phones are
     * mains-powered, so discharging is the leading indicator of the death this exists to
     * catch — carried, never graded, because a carried phone is off charge all day.
     */
    fun charging(ctx: Context): Boolean? =
        runCatching {
            val status =
                ctx
                    .registerReceiver(null, IntentFilter(Intent.ACTION_BATTERY_CHANGED))
                    ?.getIntExtra(BatteryManager.EXTRA_STATUS, -1)
                    ?: -1
            when (status) {
                BatteryManager.BATTERY_STATUS_CHARGING,
                BatteryManager.BATTERY_STATUS_FULL,
                -> true

                BatteryManager.BATTERY_STATUS_DISCHARGING,
                BatteryManager.BATTERY_STATUS_NOT_CHARGING,
                -> false

                else -> null
            }
        }.getOrNull()

    /**
     * POST one beat, blocking. Returns whether it landed; nothing depends on it.
     *
     * Blocking rather than `suspend` on purpose: the caller is [StreamService]'s own beat
     * thread, which has no coroutine scope and whose whole job is to sleep and send.
     */
    fun send(
        controlHost: String,
        device: String,
        streaming: Boolean,
        micOk: Boolean,
        ctx: Context,
    ): Boolean =
        runCatching {
            if (controlHost.isBlank()) return false
            val body = body(device, version(ctx), startedAt, streaming, charging(ctx), micOk)
            val conn =
                (
                    URL("http://$controlHost:$API_PORT/api/devices/heartbeat")
                        .openConnection() as HttpURLConnection
                ).apply {
                    requestMethod = "POST"
                    doOutput = true
                    connectTimeout = TIMEOUT_MS
                    readTimeout = TIMEOUT_MS
                    setRequestProperty("Content-Type", "application/json")
                }
            conn.outputStream.use { it.write(body.toByteArray()) }
            val code = conn.responseCode
            conn.disconnect()
            code in 200..299
        }.getOrDefault(false)
}
