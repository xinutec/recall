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
 * What recall made of an upload: the session's name and the length of the audio it
 * received, compared with the phone's copy to catch a post cut short.
 */
data class UploadedSession(
    val title: String,
    val durationMs: Long,
)

/**
 * Uploads an audio file to `POST /api/sessions`, as the web app's Upload button does,
 * making it a session. Used by the share sheet ([ShareActivity]) and the meeting
 * recorder ([MeetingUpload]). `host` is Isis, not the recorder host.
 */
object ShareUpload {
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

    /**
     * Epoch millis from SAF's `last_modified` (millis), else MediaStore's
     * `date_modified` (seconds). Zero, negative and null mean no time, not 1970.
     */
    fun modifiedMillis(lastModifiedMillis: Long?, dateModifiedSeconds: Long?): Long? =
        lastModifiedMillis?.takeIf { it > 0 }
            ?: dateModifiedSeconds?.takeIf { it > 0 }?.times(1000)

    /** Best available start time: the recorder stamp, else the file's last-modified,
     * else now. */
    fun chooseStart(name: String, modifiedMillis: Long?, now: Instant, zone: ZoneId): Instant =
        parseRecorderStart(name, zone)
            ?: modifiedMillis?.takeIf { it > 0 }?.let(Instant::ofEpochMilli)
            ?: now

    /** The session's length from the returned `start` and `end`; 0 if either is missing,
     * which [MeetingQueue.landedShort] reads as unverified. */
    fun sessionDurationMs(body: String): Long =
        runCatching {
            val json = JSONObject(body)
            Duration
                .between(
                    OffsetDateTime.parse(json.getString("start")),
                    OffsetDateTime.parse(json.getString("end")),
                ).toMillis()
        }.getOrDefault(0L)

    /** POST `file` to /api/sessions as multipart, streamed. No title: the server names
     * the session after its start.
     *
     * `token` is the device bearer ([Prefs.deviceToken]), since the phone cannot sign in
     * to Nextcloud; blank sends no header, for an ungated server. */
    suspend fun upload(
        host: String,
        file: File,
        filename: String,
        start: Instant,
        token: String = "",
    ): Result<UploadedSession> =
        withContext(Dispatchers.IO) {
            runCatching {
                val boundary = "----recall${System.nanoTime()}"
                val conn =
                    (
                        URL("${ApiBase.of(host)}/api/sessions").openConnection()
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
                        if (token.isNotBlank()) {
                            setRequestProperty("Authorization", "Bearer $token")
                        }
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
