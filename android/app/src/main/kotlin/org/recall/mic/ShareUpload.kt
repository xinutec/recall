package org.recall.mic

import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import org.json.JSONObject
import java.io.File
import java.net.HttpURLConnection
import java.net.URL
import java.time.Duration
import java.time.Instant
import java.time.LocalDateTime
import java.time.OffsetDateTime
import java.time.ZoneId
import java.time.format.DateTimeFormatter

/**
 * What recall made of an upload: the session's name, and **how long the audio it actually
 * received turned out to be**. The second one is the receipt — the server probes what
 * arrived, so comparing its length with the file still on the phone is the only way to
 * tell a complete post from one that was cut short and still parsed.
 */
data class UploadedSession(
    val title: String,
    val durationMs: Long,
)

/**
 * Uploads an audio file to `POST /api/sessions` — the same endpoint the web Upload button
 * uses — so a conversation recorded outside continuous capture becomes a recall session,
 * transcribed and diarized like any meeting. Used by both the share sheet
 * ([ShareActivity]) and the app's own recorder ([MeetingUpload]).
 *
 * The `host` is the **control host** (Isis), not the recorder host: the Mac's own web UI
 * was retired in the Isis split and its `:8000` refuses, so an upload sent to the machine
 * the PCM stream goes to has nowhere to land. Isis picks the session up and the Mac pulls
 * the blob back through the job-pull to transcribe it.
 *
 * The pure time helpers are unit-tested; the network call mirrors CaptureApi.
 */

object ShareUpload {
    private const val API_PORT = 8000 // `recall api --port 8000`, as CaptureApi

    // The recorder encodes the local start in the filename, e.g. 2026_07_03_09_50_50_1.mp3.
    private val RECORDER_STAMP = Regex("""(\d{4})_(\d{2})_(\d{2})_(\d{2})_(\d{2})_(\d{2})""")

    /** Parse the recorder's leading `YYYY_MM_DD_HH_MM_SS` stamp (in `zone`), or null if
     * the name has none or the fields aren't a real date/time. */
    fun parseRecorderStart(name: String, zone: ZoneId): Instant? =
        RECORDER_STAMP.find(name)?.destructured?.let { (y, mo, d, h, mi, s) ->
            runCatching {
                LocalDateTime
                    .of(y.toInt(), mo.toInt(), d.toInt(), h.toInt(), mi.toInt(), s.toInt())
                    .atZone(zone)
                    .toInstant()
            }.getOrNull()
        }

    /** Best available start time: the recorder stamp, else the file's last-modified,
     * else now. */
    fun chooseStart(name: String, modifiedMillis: Long?, now: Instant, zone: ZoneId): Instant =
        parseRecorderStart(name, zone)
            ?: modifiedMillis?.takeIf { it > 0 }?.let(Instant::ofEpochMilli)
            ?: now

    /**
     * How long recall says the session is, from the `start`/`end` it returns — 0 when the
     * response doesn't carry both, which [MeetingQueue.landedShort] reads as "not
     * verified" rather than as agreement.
     */
    fun sessionDurationMs(body: String): Long =
        runCatching {
            val json = JSONObject(body)
            Duration
                .between(
                    OffsetDateTime.parse(json.getString("start")),
                    OffsetDateTime.parse(json.getString("end")),
                ).toMillis()
        }.getOrDefault(0L)

    /** POST `file` to /api/sessions as multipart. Returns the created session's title on
     * success, or a failure. Streams the file (an appointment can be tens of MB).
     * No `title` is sent: the server names the session `Meeting <date> <time>` from the
     * start, and it is renamed there if it ever needs a name. */
    suspend fun upload(
        host: String,
        file: File,
        filename: String,
        start: Instant,
    ): Result<UploadedSession> =
        withContext(Dispatchers.IO) {
            runCatching {
                val boundary = "----recall${System.nanoTime()}"
                val conn =
                    (
                        URL("http://$host:$API_PORT/api/sessions").openConnection()
                            as HttpURLConnection
                    ).apply {
                        requestMethod = "POST"
                        doOutput = true
                        connectTimeout = 8000
                        readTimeout = 120_000
                        setChunkedStreamingMode(0)
                        setRequestProperty(
                            "Content-Type",
                            "multipart/form-data; boundary=$boundary",
                        )
                    }
                conn.outputStream.use { out ->
                    val header =
                        "--$boundary\r\n" +
                            "Content-Disposition: form-data; name=\"start\"\r\n\r\n" +
                            DateTimeFormatter.ISO_INSTANT.format(start) + "\r\n" +
                            "--$boundary\r\n" +
                            "Content-Disposition: form-data; name=\"audio\"; " +
                            "filename=\"$filename\"\r\n" +
                            "Content-Type: application/octet-stream\r\n\r\n"
                    out.write(header.toByteArray())
                    file.inputStream().use { it.copyTo(out) }
                    out.write("\r\n--$boundary--\r\n".toByteArray())
                }
                val code = conn.responseCode
                val stream = if (code in 200..299) conn.inputStream else conn.errorStream
                val body = stream?.bufferedReader()?.use { it.readText() } ?: ""
                conn.disconnect()
                if (code !in 200..299) error("HTTP $code")
                UploadedSession(
                    title = JSONObject(body).optString("title", filename),
                    durationMs = sessionDurationMs(body),
                )
            }
        }
}
