package org.recall.mic

import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import org.json.JSONObject
import java.io.File
import java.net.HttpURLConnection
import java.net.URL
import java.time.Instant
import java.time.LocalDateTime
import java.time.ZoneId
import java.time.format.DateTimeFormatter

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

    /** The optional `title` multipart section — empty when there is nothing to send, so
     * the server keeps its own `Meeting <date> <time>` naming rather than being handed a
     * blank one. Pure, so the wire format is unit-tested rather than trusted. */
    fun titlePart(boundary: String, title: String): String {
        val clean = title.trim()
        if (clean.isEmpty()) return ""
        return "--$boundary\r\n" +
            "Content-Disposition: form-data; name=\"title\"\r\n\r\n" +
            clean + "\r\n"
    }

    /** POST `file` to /api/sessions as multipart. Returns the created session's title on
     * success, or a failure. Streams the file (an appointment can be tens of MB).
     * A blank [title] is omitted, leaving the server's `Meeting <date> <time>` default —
     * which is all the share sheet can offer, since it has only a filename to go on. */
    suspend fun upload(
        host: String,
        file: File,
        filename: String,
        start: Instant,
        title: String = "",
    ): Result<String> =
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
                            titlePart(boundary, title) +
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
                JSONObject(body).optString("title", filename)
            }
        }
}
