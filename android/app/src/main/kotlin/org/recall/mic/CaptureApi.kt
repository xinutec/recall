package org.recall.mic

import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import org.json.JSONObject
import java.net.HttpURLConnection
import java.net.URL

/**
 * The recall *household* capture state (the whole system, not just this phone), as
 * spec-vs-status: [running]/[pausedUntil] is the mic's confirmed word, [desiredRunning]/
 * [desiredPausedUntil] is the intent (moves the instant a button is pressed), and
 * [settled] says they agree. While unsettled the UI shows "Pausing…"/"Resuming…" —
 * never a flap between the intent it just set and a not-yet-caught-up report.
 */
data class CaptureState(
    val running: Boolean,
    val pausedUntil: String?,
    val desiredRunning: Boolean,
    val desiredPausedUntil: String?,
    val settled: Boolean,
    val micReachable: Boolean,
    /** Fingerprint echoed back as ?known= to long-poll: the server holds the request
     * until the state changes. Null on an older server (fall back to plain polling). */
    val stateToken: String? = null,
)

/** One recorder's liveness for the fleet view (which mics are streaming now). */
data class SourceStatus(
    val id: String,
    val name: String,
    val kind: String,
    val active: Boolean,
    val lastActive: String?,
)

/** Parse `/api/capture`'s JSON. Pure (no I/O), so it's unit-tested. An older server
 * sends only the confirmed view; that reads as settled (desired == confirmed). */
fun parseCaptureState(body: String): CaptureState? =
    runCatching {
        val json = JSONObject(body)
        val running = json.optBoolean("running", true)
        val until = if (json.isNull("pausedUntil")) null else json.getString("pausedUntil")
        CaptureState(
            running = running,
            pausedUntil = until,
            desiredRunning = json.optBoolean("desiredRunning", running),
            desiredPausedUntil =
                when {
                    !json.has("desiredPausedUntil") -> until // old server: field absent
                    json.isNull("desiredPausedUntil") -> null
                    else -> json.getString("desiredPausedUntil")
                },
            settled = json.optBoolean("settled", true),
            micReachable = json.optBoolean("micReachable", true),
            stateToken = if (json.isNull("stateToken")) null else json.getString("stateToken"),
        )
    }.getOrNull()

/** Parse `/api/sources`'s JSON into the per-recorder list. Pure, so it's unit-tested. */
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
            )
        }
    }.getOrDefault(emptyList())

/**
 * Talks to the recall web API — the same one the web app uses — to read the global
 * capture pause (and control it), and the fleet's per-recorder liveness. Since the Isis
 * split this is the *control host* (Isis), NOT the recorder host the stream uses: the API
 * moved to Isis while the PCM ingest stayed on the Mac. The API stays up *during* a pause
 * (it's control-plane), so the app shows the true state even when the stream port is
 * closed.
 *
 * Reachable wherever the caller can reach Isis (over the VPN); if it can't, calls just
 * fail and the panels stay hidden rather than showing stale state.
 */
object CaptureApi {
    private const val API_PORT = 8000 // `recall api --port 8000`
    private const val TIMEOUT_MS = 4000

    private fun endpoint(host: String, path: String) = "http://$host:$API_PORT/api$path"

    /** With [waitS] + [known] (the last stateToken) the server long-polls: the request
     * hangs until the household state changes, so a press anywhere lands here in ~RTT.
     * The read timeout stretches to cover the hang. */
    suspend fun state(host: String, waitS: Int = 0, known: String? = null): CaptureState? {
        val query = if (waitS > 0) "?wait=$waitS&known=${known.orEmpty()}" else ""
        return get(
            endpoint(host, "/capture$query"),
            "GET",
            readTimeoutMs = TIMEOUT_MS + waitS * 1000,
        )?.let { parseCaptureState(it) }
    }

    suspend fun pause(host: String): CaptureState? =
        get(endpoint(host, "/capture/pause"), "POST")?.let { parseCaptureState(it) }

    suspend fun resume(host: String): CaptureState? =
        get(endpoint(host, "/capture/resume"), "POST")?.let { parseCaptureState(it) }

    // null = the request failed (caller should keep its last list, not blank the
    // panel); an empty list means the host genuinely has no sources.
    suspend fun sources(host: String): List<SourceStatus>? =
        get(endpoint(host, "/sources"), "GET")?.let { parseSources(it) }

    /** One request; returns the response body, or null on any failure. */
    private suspend fun get(url: String, method: String, readTimeoutMs: Int = TIMEOUT_MS): String? =
        withContext(Dispatchers.IO) {
            runCatching {
                val conn = URL(url).openConnection() as HttpURLConnection
                conn.requestMethod = method
                conn.connectTimeout = TIMEOUT_MS
                conn.readTimeout = readTimeoutMs
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
