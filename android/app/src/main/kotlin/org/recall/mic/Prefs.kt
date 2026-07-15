package org.recall.mic

import android.content.Context
import android.os.Build
import java.util.UUID

/**
 * Persisted config for the mic app, read by the foreground service and the boot
 * receiver so they share one configuration. Two hosts, because the Isis split put the
 * PCM ingest and the control API on different machines:
 *  - [host] — the *recorder* host the mic stream connects to (the Mac's ingest, on the
 *    home LAN); user-set, per-network.
 *  - [controlHost] — the *control-plane* host for the capture API (pause/resume + the
 *    fleet liveness the Devices panel shows). That's Isis, reachable over the VPN, so it
 *    defaults to [DEFAULT_CONTROL_HOST] and rarely changes.
 * The ingest port is fixed (one shared port) and the device id is derived.
 */
object Prefs {
    private const val FILE = "recall-mic"
    private const val KEY_HOST = "host"
    private const val KEY_CONTROL_HOST = "control_host"
    private const val KEY_DEVICE_ID = "device_id"

    // Legacy key from the manual-device-id version; adopted for source continuity.
    private const val KEY_LEGACY_SOURCE_ID = "source_id"
    private const val KEY_ENABLED = "enabled"
    private const val MAX_ID_LEN = 40

    // Isis (the fleet control plane) over WireGuard: a stable address, so it's the
    // out-of-the-box default and existing installs self-heal without reconfiguration.
    // The stream still goes to the recorder [host]; only the API moved here.
    const val DEFAULT_CONTROL_HOST = "10.100.0.2"

    private fun prefs(ctx: Context) = ctx.getSharedPreferences(FILE, Context.MODE_PRIVATE)

    private fun sanitize(raw: String): String =
        raw.lowercase().replace(Regex("[^a-z0-9]+"), "-").trim('-')

    fun host(ctx: Context): String = prefs(ctx).getString(KEY_HOST, "") ?: ""

    /** The capture-API host (Isis). Empty/unset falls back to [DEFAULT_CONTROL_HOST], so
     * the pause controls and Devices panel work out of the box against the fleet. */
    fun controlHost(ctx: Context): String =
        (prefs(ctx).getString(KEY_CONTROL_HOST, "") ?: "").ifEmpty { DEFAULT_CONTROL_HOST }

    fun saveControlHost(ctx: Context, controlHost: String) {
        prefs(ctx).edit().putString(KEY_CONTROL_HOST, controlHost).apply()
    }

    /** This device's stable recall source id, announced in the stream handshake.
     * Resolved once and persisted — nothing for the user to set; renamable in the web
     * UI. A phone upgraded from the manual-device-id version keeps that id (so its
     * recording history stays one source); a fresh phone derives model + random suffix
     * (so two same-model phones differ). */
    fun deviceId(ctx: Context): String {
        val existing = prefs(ctx).getString(KEY_DEVICE_ID, null)
        if (existing != null) return existing
        val legacy = prefs(ctx).getString(KEY_LEGACY_SOURCE_ID, null)?.let(::sanitize)
        val id =
            (
                legacy?.takeIf { it.isNotEmpty() }
                    ?: "${sanitize(Build.MODEL).ifEmpty { "device" }}-" +
                    UUID.randomUUID().toString().take(8)
            ).take(MAX_ID_LEN)
        prefs(ctx).edit().putString(KEY_DEVICE_ID, id).apply()
        return id
    }

    /** Whether streaming should be running — set true on Start, drives boot restart. */
    fun enabled(ctx: Context): Boolean = prefs(ctx).getBoolean(KEY_ENABLED, false)

    fun save(ctx: Context, host: String, enabled: Boolean) {
        prefs(ctx)
            .edit()
            .putString(KEY_HOST, host)
            .putBoolean(KEY_ENABLED, enabled)
            .apply()
    }
}
