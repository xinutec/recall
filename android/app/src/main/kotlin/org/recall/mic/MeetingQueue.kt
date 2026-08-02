package org.recall.mic

import android.content.Context
import android.os.Environment
import android.util.Log
import org.json.JSONObject
import java.io.File
import java.time.Instant
import java.time.LocalDateTime
import java.time.ZoneId
import java.time.format.DateTimeFormatter

/** A finished recording on disk, waiting for the host to answer. */
data class PendingRecording(
    val audio: File,
    val title: String,
    val start: Instant,
)

/**
 * The on-disk queue of meeting recordings: where they live, what they're called, and the
 * metadata that has to survive the app dying between the recording and its upload.
 *
 * Offline-first is the whole point — a meeting happens where the recall host is
 * unreachable, and some guest networks block the VPN outright, so there may be no route
 * home from the building at all. The recording is therefore a file first and an upload
 * second, and nothing about it may live only in memory.
 *
 * Each recording is a pair: `meeting-<local stamp>.ogg` and a `.ogg.json` sidecar holding
 * the title and the true start instant. The sidecar is written *before* the first audio
 * frame, so a recording that ends in a crash still knows what it is. Where the sidecar is
 * missing anyway, [startFromName] recovers the start from the filename and the title falls
 * back to empty — which the server renders as `Meeting <date> <time>`, not an error.
 */
object MeetingQueue {
    const val AUDIO_SUFFIX = ".ogg"
    private const val DIR = "meetings"
    private const val SIDECAR_SUFFIX = ".json"
    private const val KEY_TITLE = "title"
    private const val KEY_START = "start"
    private const val TAG = "recall.meeting"

    // Local wall-clock in the name: this is what a human scanning the directory over USB
    // reads, and it's the fallback start if the sidecar is lost.
    private val STAMP = DateTimeFormatter.ofPattern("yyyyMMdd-HHmmss")
    private val NAME = Regex("""^meeting-(\d{8})-(\d{6})""")

    /**
     * `Android/data/org.recall.mic/files/Music/meetings` — app-private, so no storage
     * permission and no scoped-storage dance, but *visible over USB*, unlike `filesDir`.
     * A recording whose upload never succeeds can still be pulled off the phone by hand.
     */
    fun dir(ctx: Context): File =
        File(ctx.getExternalFilesDir(Environment.DIRECTORY_MUSIC), DIR).apply { mkdirs() }

    fun fileName(start: Instant, zone: ZoneId): String =
        "meeting-${STAMP.format(start.atZone(zone))}$AUDIO_SUFFIX"

    /** The start encoded in [name] by [fileName], or null if it isn't one of ours. */
    fun startFromName(name: String, zone: ZoneId): Instant? =
        NAME.find(name)?.destructured?.let { (d, t) ->
            runCatching {
                LocalDateTime
                    .of(
                        d.substring(0, 4).toInt(),
                        d.substring(4, 6).toInt(),
                        d.substring(6, 8).toInt(),
                        t.substring(0, 2).toInt(),
                        t.substring(2, 4).toInt(),
                        t.substring(4, 6).toInt(),
                    ).atZone(zone)
                    .toInstant()
            }.getOrNull()
        }

    private fun sidecar(audio: File): File = File(audio.path + SIDECAR_SUFFIX)

    /** Record what this file is, before it has any audio in it. */
    fun writeSidecar(audio: File, title: String, start: Instant) {
        runCatching {
            sidecar(audio).writeText(
                JSONObject()
                    .put(KEY_TITLE, title)
                    .put(KEY_START, DateTimeFormatter.ISO_INSTANT.format(start))
                    .toString(),
            )
        }.onFailure { Log.w(TAG, "could not write sidecar for ${audio.name}", it) }
    }

    /** Title and start from the sidecar, or null if it's absent or unreadable. */
    fun readSidecar(audio: File): Pair<String, Instant>? =
        runCatching {
            val json = JSONObject(sidecar(audio).readText())
            json.optString(KEY_TITLE) to Instant.parse(json.getString(KEY_START))
        }.getOrNull()

    /**
     * Everything waiting to upload, oldest first. Zero-length files are skipped: a
     * `MediaRecorder` that was stopped before it wrote a page leaves one, and posting it
     * would only earn a 400 from the server's ffprobe.
     */
    fun pending(dir: File, zone: ZoneId): List<PendingRecording> =
        (dir.listFiles() ?: emptyArray())
            .filter { it.isFile && it.name.endsWith(AUDIO_SUFFIX) && it.length() > 0 }
            .sortedBy { it.name }
            .map { audio ->
                val meta = readSidecar(audio)
                PendingRecording(
                    audio = audio,
                    title = meta?.first ?: "",
                    start =
                        meta?.second
                            ?: startFromName(audio.name, zone)
                            ?: Instant.ofEpochMilli(audio.lastModified()),
                )
            }

    /** Drop a recording and its sidecar — called once the host has it. */
    fun complete(recording: PendingRecording) {
        sidecar(recording.audio).delete()
        recording.audio.delete()
    }

    /** Discard a recording that never got any audio (stopped instantly, or failed). */
    fun discard(audio: File) {
        sidecar(audio).delete()
        audio.delete()
    }
}
