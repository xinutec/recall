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
 * "Still here", once an hour, whether or not anything streams (#837): audio alone cannot
 * tell a dead app from a quiet room or a pause.
 *
 * Sent to Isis, reachable from anywhere over WireGuard, so a phone that is out still
 * beats; the recorder's LAN address is the fallback. Separate from [OutboxReport], which
 * runs on the upload schedule and only on meeting phones.
 *
 * Only while the service runs: a stopped app records nothing and must not look alive.
 * Best-effort and silent.
 */
object Heartbeat {
    private const val TIMEOUT_MS = 8000

    /** How often to beat; equal to `recalld::devices::BEAT_EVERY_MINUTES`, which the
     * grader's thresholds are multiples of. */
    const val EVERY_MINUTES = 60L

    /** First retry after a failed beat; doubles per failure up to [EVERY_MINUTES] (#886). */
    private const val RETRY_BASE_MINUTES = 1L

    /** When this process started (on class load, by the service), so restarts show. */
    val startedAt: Instant = Instant.now()

    /**
     * Minutes until the next beat: the full cadence after a success, a backoff after a
     * failure, never more often than hourly once the backoff reaches the cadence.
     */
    fun nextDelayMinutes(consecutiveFailures: Int): Long {
        if (consecutiveFailures <= 0) return EVERY_MINUTES
        // Doubling, not shifting: the counter grows without bound in a dead spot, and
        // `1L shl 63` would wrap.
        var delay = RETRY_BASE_MINUTES
        repeat(consecutiveFailures - 1) {
            if (delay >= EVERY_MINUTES) return EVERY_MINUTES
            delay *= 2
        }
        return minOf(delay, EVERY_MINUTES)
    }

    /** App version and build, so a restart into a new build reads as a deploy. */
    fun version(ctx: Context): String =
        runCatching {
            val info = ctx.packageManager.getPackageInfo(ctx.packageName, 0)
            @Suppress("DEPRECATION") // versionCode is display-only; longVersionCode needs API 28
            "${info.versionName} (${info.versionCode})"
        }.getOrDefault("?")

    /** A beat's JSON, the server's `HeartbeatIn`. */
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
            // A running app that cannot open its mic (#887).
            .put("micOk", micOk)
            // Absent when unknown, rather than a guess.
            .apply { if (charging != null) put("charging", charging) }
            .toString()

    /**
     * True on mains, false on battery, null if unknown. For a room phone, discharging
     * warns of its death; reported, not graded, since a carried phone discharges all day.
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

    /** POST one beat, blocking (the caller is a plain thread). Whether it landed. */
    fun send(
        controlHost: String,
        lanHost: String,
        device: String,
        streaming: Boolean,
        micOk: Boolean,
        ctx: Context,
    ): Boolean {
        val body = body(device, version(ctx), startedAt, streaming, charging(ctx), micOk)
        // Isis first, then the recorder's LAN address, where the Mac relays beats (#888):
        // a phone at home with its tunnel off still records, so must not read as dead.
        for (host in hostsToTry(controlHost, lanHost)) {
            if (post(body, host)) return true
        }
        return false
    }

    /** The hosts to try, in order, blanks and duplicates dropped. */
    fun hostsToTry(controlHost: String, lanHost: String): List<String> =
        listOf(controlHost, lanHost).filter { it.isNotBlank() }.distinct()

    private fun post(body: String, host: String): Boolean =
        runCatching {
            val conn =
                (
                    URL("${ApiBase.of(host)}/api/devices/heartbeat")
                        .openConnection() as HttpURLConnection
                ).apply {
                    requestMethod = "POST"
                    doOutput = true
                    connectTimeout = TIMEOUT_MS
                    readTimeout = TIMEOUT_MS
                    setRequestProperty("Content-Type", "application/json")
                }
            conn.outputStream.use { it.write(body.toByteArray()) }
            // Isis answers 200, the relay 204.
            val code = conn.responseCode
            conn.disconnect()
            code in 200..299
        }.getOrDefault(false)
}
