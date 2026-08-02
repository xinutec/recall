package org.recall.mic

import android.content.Context
import android.os.Environment
import java.io.File
import java.time.Instant
import java.time.LocalDateTime
import java.time.ZoneId
import java.time.format.DateTimeFormatter

/** A finished recording on disk, waiting for the host to answer. */
data class PendingRecording(
    val audio: File,
    val start: Instant,
)

/**
 * The recordings on the phone: where they live and what they're called.
 *
 * Offline-first is the whole point — a meeting happens where the recall host is
 * unreachable, and some guest networks block the VPN outright, so there may be no route
 * home from the building at all. The recording is therefore a file first and an upload
 * second, and nothing about it may live only in memory.
 *
 * **One file per recording, named for when it was made** — `meeting-<local stamp>.ogg`,
 * the same convention the third-party recorder used. There is no title and no sidecar:
 * the only thing worth knowing about a recording before it is transcribed is when it
 * happened, and the filename already says that. recall names the session
 * `Meeting <date> <time>` and it can be renamed there, where the transcript is to hand
 * and the name can be chosen for what the meeting turned out to be.
 *
 * **Two directories, because approval is a decision and decisions must survive a reboot.**
 * A finished recording stays in `meetings/`, where it can be played back and kept or
 * deleted, and *nothing sends it*. Pressing Upload moves it into `meetings/outbox/`, which
 * is the only place [MeetingUpload] looks. One rename within one directory tree, so it is
 * atomic: a recording is either awaiting a decision or acting on one, never both and
 * never neither.
 */
object MeetingQueue {
    const val AUDIO_SUFFIX = ".ogg"
    private const val DIR = "meetings"
    private const val OUTBOX = "outbox"

    // Local wall-clock in the name: this is what a human scanning the directory over USB
    // reads, and it is the recording's start. Ambiguous for one repeated hour when the
    // clocks go back, where `atZone` takes the earlier offset — an hour's error on a
    // recording made at 01:30 on that one night, which is cheaper than carrying an offset
    // in every filename to prevent it.
    private val STAMP = DateTimeFormatter.ofPattern("yyyyMMdd-HHmmss")
    private val NAME = Regex("""^meeting-(\d{8})-(\d{6})""")

    /**
     * `Android/data/org.recall.mic/files/Music/meetings` — app-private, so no storage
     * permission and no scoped-storage dance, but *visible over USB*, unlike `filesDir`.
     * A recording whose upload never succeeds can still be pulled off the phone by hand.
     */
    fun dir(ctx: Context): File =
        File(ctx.getExternalFilesDir(Environment.DIRECTORY_MUSIC), DIR).apply { mkdirs() }

    /** Recordings the user has approved for upload — the only ones that get sent. */
    fun outbox(ctx: Context): File = File(dir(ctx), OUTBOX).apply { mkdirs() }

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

    /**
     * The recordings in one directory, oldest first. Zero-length files are skipped: a
     * `MediaRecorder` that was stopped before it wrote a page leaves one, and posting it
     * would only earn a 400 from the server's ffprobe.
     *
     * A file whose name doesn't parse falls back to its mtime, so something copied in by
     * hand still lands at a plausible time rather than being hidden.
     */
    fun list(dir: File, zone: ZoneId): List<PendingRecording> =
        (dir.listFiles() ?: emptyArray())
            .filter { it.isFile && it.name.endsWith(AUDIO_SUFFIX) && it.length() > 0 }
            .sortedBy { it.name }
            .map { audio ->
                PendingRecording(
                    audio = audio,
                    start =
                        startFromName(audio.name, zone)
                            ?: Instant.ofEpochMilli(audio.lastModified()),
                )
            }

    /**
     * Move a recording into [outbox] — the act of approving it for upload. Returns it at
     * its new home, or null if the move failed, in which case it stays held rather than
     * quietly becoming a recording nobody is going to send.
     */
    fun approve(recording: PendingRecording, outbox: File): PendingRecording? {
        val moved = File(outbox, recording.audio.name)
        if (!recording.audio.renameTo(moved)) return null
        return recording.copy(audio = moved)
    }

    /** Drop a recording — once the host has it, or on the user's say-so. */
    fun complete(recording: PendingRecording) {
        recording.audio.delete()
    }

    /** Discard a recording that never got any audio (stopped instantly, or failed). */
    fun discard(audio: File) {
        audio.delete()
    }
}
