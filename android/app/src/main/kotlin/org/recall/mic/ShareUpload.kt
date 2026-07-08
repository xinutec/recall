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
 * Uploads a shared audio file to the recall host's `POST /api/sessions` — the same
 * endpoint the web Upload button uses — so a conversation recorded elsewhere (the mp3
 * recorder's Share sheet) becomes a recall session, transcribed and diarized like any
 * meeting. The pure time helpers are unit-tested; the network call mirrors CaptureApi.
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

    /** POST `file` to /api/sessions as multipart. Returns the created session's title on
     * success, or a failure. Streams the file (an appointment can be tens of MB). */
    suspend fun upload(host: String, file: File, filename: String, start: Instant): Result<String> =
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
                JSONObject(body).optString("title", filename)
            }
        }
}
