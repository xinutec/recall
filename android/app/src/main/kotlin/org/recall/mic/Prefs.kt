package org.recall.mic

import android.content.Context
import android.os.Build
import java.util.UUID

/**
 * The app's saved settings, shared by the UI, the service and the boot receiver. Two
 * hosts: [host], the recorder the PCM stream goes to (the Mac, on the home LAN), and
 * [controlHost], Isis, for the capture API and the devices panel.
 */
object Prefs {
    private const val FILE = "recall-mic"
    private const val KEY_HOST = "host"
    private const val KEY_CONTROL_HOST = "control_host"
    private const val KEY_DEVICE_ID = "device_id"
    private const val KEY_DEVICE_TOKEN = "device_token"
    private const val KEY_INGEST_TOKEN = "ingest_token"

    // From the version where the id was typed in; kept so the source stays the same.
    private const val KEY_LEGACY_SOURCE_ID = "source_id"
    private const val KEY_ENABLED = "enabled"
    private const val MAX_ID_LEN = 40

    // Isis, a name served on the VPN only. Read through [ApiBase], which also takes a
    // bare host.
    const val DEFAULT_CONTROL_HOST = "https://recall.xinutec.org"

    // The previous default (#1799), stored by installs that saved the settings; read
    // as unset.
    private const val OLD_DEFAULT_CONTROL_HOST = "10.100.0.2"

    // recalld's ingest, the same server. Not a setting.
    const val INGEST_BASE = "https://recall.xinutec.org"

    private fun prefs(ctx: Context) = ctx.getSharedPreferences(FILE, Context.MODE_PRIVATE)

    private fun sanitize(raw: String): String =
        raw.lowercase().replace(Regex("[^a-z0-9]+"), "-").trim('-')

    fun host(ctx: Context): String = prefs(ctx).getString(KEY_HOST, "") ?: ""

    /** The capture-API host; [DEFAULT_CONTROL_HOST] when unset. */
    fun controlHost(ctx: Context): String =
        effectiveControlHost(prefs(ctx).getString(KEY_CONTROL_HOST, "") ?: "")

    /** A stored control host; unset or the old default means [DEFAULT_CONTROL_HOST]. */
    fun effectiveControlHost(stored: String): String =
        if (stored.isEmpty() || stored == OLD_DEFAULT_CONTROL_HOST) DEFAULT_CONTROL_HOST else stored

    fun saveControlHost(ctx: Context, controlHost: String) {
        prefs(ctx).edit().putString(KEY_CONTROL_HOST, controlHost).apply()
    }

    /**
     * The bearer for uploading a recording (`RECALL_DEVICE_TOKEN` on the server); empty
     * for an ungated server. It authorises `POST /api/sessions` only, so a lost phone
     * costs uploads, not the archive.
     */
    fun deviceToken(ctx: Context): String = prefs(ctx).getString(KEY_DEVICE_TOKEN, "") ?: ""

    fun saveDeviceToken(ctx: Context, token: String) {
        prefs(ctx).edit().putString(KEY_DEVICE_TOKEN, token.trim()).apply()
    }

    /** The bearer for `PUT`ting this phone's own segments to recalld, and nothing
     * else. Empty sends no header. */
    fun ingestToken(ctx: Context): String = prefs(ctx).getString(KEY_INGEST_TOKEN, "") ?: ""

    fun saveIngestToken(ctx: Context, token: String) {
        prefs(ctx).edit().putString(KEY_INGEST_TOKEN, token.trim()).apply()
    }

    fun ingestBase(
        @Suppress("UNUSED_PARAMETER") ctx: Context, // like the other readers
    ): String = INGEST_BASE

    /** This phone's source id, sent in the handshake; made once and kept. The typed-in
     * id of older versions if there is one, else the model plus a random suffix. */
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

    /** Whether streaming should run: set by Start and Stop, read at boot. */
    fun enabled(ctx: Context): Boolean = prefs(ctx).getBoolean(KEY_ENABLED, false)

    fun save(ctx: Context, host: String, enabled: Boolean) {
        prefs(ctx)
            .edit()
            .putString(KEY_HOST, host)
            .putBoolean(KEY_ENABLED, enabled)
            .apply()
    }
}
