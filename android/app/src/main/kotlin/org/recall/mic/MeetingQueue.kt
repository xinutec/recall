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
 * The outbox for the server: how much is undelivered, the oldest, how much is failing,
 * and why. Sent after every upload pass, empty ones included, so the check can return
 * to green.
 */
data class OutboxState(
    val queued: Int,
    val oldestStart: Instant?,
    val failing: Int,
    val reason: String?,
)

/**
 * The recordings on the phone: where they live and what they are called. A meeting
 * usually happens where recall is unreachable, so a recording is a file first and an
 * upload second, and nothing about it lives only in memory.
 *
 * One file per recording, `meeting-<local stamp>.ogg`, named for when it was made; recall
 * names the session after the same time, and it is renamed there. Beside it may be
 * `<name>.ogg.failure` ([FAILURE_SUFFIX]): why the last delivery failed, on disk because
 * [MeetingUpload] runs when the app is gone.
 *
 * A recording's state is the directory it is in, since a rename cannot half-happen and
 * survives a reboot:
 *
 * | directory              | meaning                                                |
 * |------------------------|--------------------------------------------------------|
 * | `meetings/`            | held: listened to or not, nothing sends it              |
 * | `meetings/outbox/`     | approved — the only place [MeetingUpload] looks          |
 * | `meetings/uploaded/`   | recall has it, and its length matches this copy          |
 * | `meetings/unverified/` | recall has it, but the two lengths don't agree           |
 *
 * The phone's copy is deleted only when the user says so: a body cut short mid-post still
 * parses and gets a 2xx, and then the phone holds the only complete recording.
 */
object MeetingQueue {
    const val AUDIO_SUFFIX = ".ogg"

    /** After the audio's full name, so `list`, which matches [AUDIO_SUFFIX] at the end,
     * never takes a note for a recording. */
    const val FAILURE_SUFFIX = ".failure"
    private const val DIR = "meetings"
    private const val OUTBOX = "outbox"
    private const val UPLOADED = "uploaded"
    private const val UNVERIFIED = "unverified"

    // How much shorter recall's copy may be and still count as the same recording. Two
    // probes of one file differ by tens of milliseconds; a cut-short post loses seconds.
    private const val LENGTH_TOLERANCE_MS = 1_500L

    // Local time, for someone reading the directory over USB. In the repeated hour when
    // the clocks go back, `atZone` takes the earlier offset: an hour off, that one night.
    private val STAMP = DateTimeFormatter.ofPattern("yyyyMMdd-HHmmss")
    private val NAME = Regex("""^meeting-(\d{8})-(\d{6})""")

    /**
     * `Android/data/org.recall.mic/files/Music/meetings`: app-private, so no storage
     * permission, but unlike `filesDir` visible over USB, so a recording can be pulled
     * off by hand.
     */
    fun dir(ctx: Context): File =
        File(ctx.getExternalFilesDir(Environment.DIRECTORY_MUSIC), DIR).apply { mkdirs() }

    /** Approved for upload; the only ones sent. */
    fun outbox(ctx: Context): File = File(dir(ctx), OUTBOX).apply { mkdirs() }

    /** Delivered, and recall's copy is as long as this one. */
    fun uploaded(ctx: Context): File = File(dir(ctx), UPLOADED).apply { mkdirs() }

    /** Delivered, but the lengths disagree. */
    fun unverified(ctx: Context): File = File(dir(ctx), UNVERIFIED).apply { mkdirs() }

    /**
     * Whether recall's copy is shorter than the phone's beyond the tolerance. Longer is
     * two decoders rounding differently. An unknown length (0) counts as short: the upload
     * is not verified.
     */
    fun landedShort(
        localMs: Long,
        remoteMs: Long,
        toleranceMs: Long = LENGTH_TOLERANCE_MS,
    ): Boolean {
        if (localMs <= 0 || remoteMs <= 0) return true
        return remoteMs < localMs - toleranceMs
    }

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
     * The recordings in one directory, oldest first, skipping empty files (a recorder
     * stopped before its first page). A name that does not parse is dated by its mtime.
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
     * Move a recording into [target]: every state change. Null if the rename failed,
     * leaving it where it was. Clears the failure note, since a move ends the attempt.
     */
    fun moveTo(recording: PendingRecording, target: File): PendingRecording? {
        val moved = File(target, recording.audio.name)
        if (!recording.audio.renameTo(moved)) return null
        clearFailure(recording.audio)
        return recording.copy(audio = moved)
    }

    /** Only on the user's say-so. */
    fun delete(recording: PendingRecording) {
        clearFailure(recording.audio)
        recording.audio.delete()
    }

    /** Why the last delivery of [audio] failed, written beside it. */
    fun noteFailure(audio: File, reason: String) {
        runCatching { failureFile(audio).writeText(reason) }
    }

    fun clearFailure(audio: File) {
        runCatching { failureFile(audio).delete() }
    }

    /** The noted reason, or null if the last attempt succeeded or none has been made. */
    fun failure(audio: File): String? =
        runCatching { failureFile(audio).takeIf { it.isFile }?.readText() }
            .getOrNull()
            ?.takeIf { it.isNotBlank() }

    private fun failureFile(audio: File) = File(audio.parentFile, audio.name + FAILURE_SUFFIX)

    /**
     * What the phone still holds that it was told to send (#77). `oldestStart` is the
     * recording's start, the only time the phone keeps; `reason` the newest failure.
     */
    fun state(outbox: File, zone: ZoneId): OutboxState {
        val queue = list(outbox, zone)
        val failures = queue.mapNotNull { failure(it.audio) }
        return OutboxState(
            queued = queue.size,
            oldestStart = queue.minByOrNull { it.start }?.start,
            failing = failures.size,
            reason = failures.lastOrNull(),
        )
    }

    /** Discard a recording that never got any audio (stopped instantly, or failed). */
    fun discard(audio: File) {
        audio.delete()
    }
}
