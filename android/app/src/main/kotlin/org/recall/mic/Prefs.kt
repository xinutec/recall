package org.recall.mic

import android.content.Context
import android.os.Build
import java.util.UUID

/**
 * The recall host the mic stream connects to, persisted so the foreground service
 * and the boot receiver both read the same configuration. Only the host is set by
 * the user — the ingest port is fixed (one shared port) and the device id is derived.
 */
object Prefs {
    private const val FILE = "recall-mic"
    private const val KEY_HOST = "host"
    private const val KEY_DEVICE_ID = "device_id"

    // Legacy key from the manual-device-id version; adopted for source continuity.
    private const val KEY_LEGACY_SOURCE_ID = "source_id"
    private const val KEY_ENABLED = "enabled"
    private const val MAX_ID_LEN = 40

    private fun prefs(ctx: Context) = ctx.getSharedPreferences(FILE, Context.MODE_PRIVATE)

    private fun sanitize(raw: String): String =
        raw.lowercase().replace(Regex("[^a-z0-9]+"), "-").trim('-')

    fun host(ctx: Context): String = prefs(ctx).getString(KEY_HOST, "") ?: ""

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
