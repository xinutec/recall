package org.recall.mic

import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import org.json.JSONObject
import java.net.HttpURLConnection
import java.net.URL
import java.time.format.DateTimeFormatter

/**
 * Tell the fleet what this phone is still holding.
 *
 * An approved recording the phone cannot deliver was the one piece of state nothing
 * outside the phone could see. The meeting recorder 401ed from the day it was written;
 * WorkManager retried out to a +1h23m backoff, the audio stayed safe, and the screen
 * said "N recordings waiting to upload" — which reads the same as "not home yet" and as
 * "not approved yet". [MeetingUpload] now writes *why* on the row ([UploadFailure]), but
 * that only helps somebody already holding the phone and wondering (#77).
 *
 * ⚠ **Silence is not health, and this cannot pretend otherwise.** A phone with no route
 * home cannot report either — so the absence of a report has to be the finding, judged
 * at the other end, exactly as fleetwatch already treats a producer that stops. Sending
 * the same thing more insistently would not help, and would flatten the one distinction
 * worth keeping: uploads failing while the fleet is reachable is a fault, and uploads
 * not happening because you are out of the house is a Tuesday.
 *
 * Best-effort and silent: this is a status report about failures, and a status report
 * that raises its own failures would be the tail wagging the dog. The mirror of
 * `recall doctor --post` on the Mac, which does the same thing for the same reason.
 */
object OutboxReport {
    private const val API_PORT = 8000 // `recall api --port 8000`, as ShareUpload

    /** POST the outbox state. Returns whether it landed; nothing depends on it. */
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
                        URL("http://$host:$API_PORT/api/devices/outbox").openConnection()
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
