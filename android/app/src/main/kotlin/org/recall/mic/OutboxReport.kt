package org.recall.mic

import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import org.json.JSONObject
import java.net.HttpURLConnection
import java.net.URL
import java.time.format.DateTimeFormatter

/**
 * Tell the server what approved recordings this phone still holds (#77), so a stuck
 * upload is visible without the phone in hand.
 *
 * A phone with no route home cannot report either, so a missing report is judged on the
 * server. Failing while the server is reachable is a fault; not uploading while out of
 * the house is not. Best-effort and silent.
 */
object OutboxReport {
    /** POST the outbox state; whether it landed. */
    suspend fun send(
        host: String,
        device: String,
        state: OutboxState,
        token: String = "",
    ): Boolean =
        withContext(Dispatchers.IO) {
            runCatching {
                val body =
                    JSONObject()
                        .put("device", device)
                        .put("queued", state.queued)
                        .put(
                            "oldestQueuedAt",
                            state.oldestStart?.let(DateTimeFormatter.ISO_INSTANT::format)
                                ?: JSONObject.NULL,
                        ).put("failing", state.failing)
                        .put("reason", state.reason ?: JSONObject.NULL)
                        .toString()
                val conn =
                    (
                        URL("${ApiBase.of(host)}/api/devices/outbox").openConnection()
                            as HttpURLConnection
                    ).apply {
                        requestMethod = "POST"
                        doOutput = true
                        connectTimeout = 8000
                        readTimeout = 8000
                        setRequestProperty("Content-Type", "application/json")
                        if (token.isNotBlank()) {
                            setRequestProperty("Authorization", "Bearer $token")
                        }
                    }
                conn.outputStream.use { it.write(body.toByteArray()) }
                val code = conn.responseCode
                conn.disconnect()
                code in 200..299
            }.getOrDefault(false)
        }
}
