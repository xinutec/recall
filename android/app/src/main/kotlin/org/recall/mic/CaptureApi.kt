package org.recall.mic

import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import org.json.JSONObject
import java.net.HttpURLConnection
import java.net.URL

/**
 * The household's capture state, not just this phone's. [running] and [pausedUntil] are
 * what the mic confirmed; the `desired` pair what was asked, changed at the press;
 * [settled] that they agree. Until then the UI says "Pausing…" or "Resuming…".
 */
data class CaptureState(
    val running: Boolean,
    val pausedUntil: String?,
    val desiredRunning: Boolean,
    val desiredPausedUntil: String?,
    val settled: Boolean,
    val micReachable: Boolean,
    /** Sent back as `?known=` to long-poll. */
    val stateToken: String,
)

/**
 * One recorder's liveness. [active]: someone is being recorded audibly, so a silent
 * room reads inactive. [recording]: audio is arriving, whatever is on it.
 */
data class SourceStatus(
    val id: String,
    val name: String,
    val kind: String,
    val active: Boolean,
    val lastActive: String?,
    val recording: Boolean,
    val lastDelivered: String?,
)

/** Parse `/api/capture`'s JSON; null if a flag or the token is missing. */
fun parseCaptureState(body: String): CaptureState? =
    runCatching {
        val json = JSONObject(body)

        fun instant(key: String) = if (json.isNull(key)) null else json.getString(key)
        CaptureState(
            running = json.getBoolean("running"),
            pausedUntil = instant("pausedUntil"),
            desiredRunning = json.getBoolean("desiredRunning"),
            desiredPausedUntil = instant("desiredPausedUntil"),
            settled = json.getBoolean("settled"),
            micReachable = json.getBoolean("micReachable"),
            stateToken = json.getString("stateToken"),
        )
    }.getOrNull()

/** Parse `/api/sources`'s JSON. */
fun parseSources(body: String): List<SourceStatus> =
    runCatching {
        val items = JSONObject(body).getJSONArray("items")
        (0 until items.length()).map { i ->
            val o = items.getJSONObject(i)
            SourceStatus(
                id = o.getString("id"),
                name = o.getString("name"),
                kind = o.getString("kind"),
                active = o.optBoolean("active", false),
                lastActive = if (o.isNull("lastActive")) null else o.optString("lastActive"),
                recording = o.optBoolean("recording", false),
                lastDelivered =
                    if (o.isNull("lastDelivered")) null else o.optString("lastDelivered"),
            )
        }
    }.getOrDefault(emptyList())

/**
 * The web API on Isis: the household pause, read and set, and the recorders' liveness.
 * It answers during a pause, when the recorder's port is closed. A failed call returns
 * null, and the panels stay hidden.
 */
object CaptureApi {
    private const val TIMEOUT_MS = 4000

    private fun endpoint(host: String, path: String) = "${ApiBase.of(host)}/api$path"

    /** With [waitS] and [known] (the last stateToken), a long poll: the server answers
     * when the state changes, so the read timeout covers the wait. */
    suspend fun state(
        host: String,
        token: String,
        waitS: Int = 0,
        known: String? = null,
    ): CaptureState? {
        val query = if (waitS > 0) "?wait=$waitS&known=${known.orEmpty()}" else ""
        return get(
            endpoint(host, "/capture$query"),
            "GET",
            token,
            readTimeoutMs = TIMEOUT_MS + waitS * 1000,
        )?.let { parseCaptureState(it) }
    }

    suspend fun pause(host: String, token: String): CaptureState? =
        get(endpoint(host, "/capture/pause"), "POST", token)?.let { parseCaptureState(it) }

    suspend fun resume(host: String, token: String): CaptureState? =
        get(endpoint(host, "/capture/resume"), "POST", token)?.let { parseCaptureState(it) }

    /** The recorders of whoever owns this phone ([token] says whose); null: the request
     * failed; empty: none. */
    suspend fun sources(host: String, token: String): List<SourceStatus>? =
        get(endpoint(host, "/sources"), "GET", token)?.let { parseSources(it) }

    /** One request; returns the response body, or null on any failure. [token], this
     * phone's own (`Prefs.ingestToken`), says whose phone asks; blank sends none. */
    private suspend fun get(
        url: String,
        method: String,
        token: String,
        readTimeoutMs: Int = TIMEOUT_MS,
    ): String? =
        withContext(Dispatchers.IO) {
            runCatching {
                val conn = URL(url).openConnection() as HttpURLConnection
                conn.requestMethod = method
                conn.connectTimeout = TIMEOUT_MS
                conn.readTimeout = readTimeoutMs
                if (token.isNotBlank()) conn.setRequestProperty("Authorization", "Bearer $token")
                if (method == "POST") {
                    conn.doOutput = true
                    conn.outputStream.close() // empty body; the endpoints take none
                }
                val body = conn.inputStream.bufferedReader().use { it.readText() }
                conn.disconnect()
                body
            }.getOrNull()
        }
}
