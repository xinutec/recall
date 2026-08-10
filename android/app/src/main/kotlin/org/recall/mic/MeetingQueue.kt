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
 * the same convention the third-party recorder used. There is no title: the only thing
 * worth knowing about a recording before it is transcribed is when it happened, and the
 * filename already says that. recall names the session `Meeting <date> <time>` and it
 * can be renamed there, where the transcript is to hand and the name can be chosen for
 * what the meeting turned out to be.
 *
 * **The one exception is [FAILURE_SUFFIX]**, `<name>.ogg.failure`, holding why the last
 * delivery attempt didn't land. It is not metadata about the recording — it is state
 * about an attempt, it is deleted the moment one succeeds, and it has to be on disk for
 * the same reason the audio is: [MeetingUpload] runs under WorkManager when the app is
 * gone, so a reason kept in memory is lost exactly when someone opens the screen to ask.
 *
 * **A recording's state is which directory it is in**, because every one of those states
 * is a decision or a verdict that has to survive a reboot, and a rename is the only way
 * to change one that can't half-happen:
 *
 * | directory              | meaning                                                |
 * |------------------------|--------------------------------------------------------|
 * | `meetings/`            | held: listened to or not, nothing sends it              |
 * | `meetings/outbox/`     | approved — the only place [MeetingUpload] looks          |
 * | `meetings/uploaded/`   | recall has it, and its length matches this copy          |
 * | `meetings/unverified/` | recall has it, but the two lengths don't agree           |
 *
 * Nothing is ever deleted by getting to the end of that list. The phone's copy goes when
 * the user says so and at no other time: a 2xx means recall *received* something, and the
 * one failure that survives every check upstream — a body cut short mid-post, which still
 * parses and so still returns 2xx — is exactly the one where the phone holds the only
 * complete recording.
 */
object MeetingQueue {
    const val AUDIO_SUFFIX = ".ogg"

    /** Suffixed onto the audio's full name, so `list` — which matches [AUDIO_SUFFIX] at
     * the end — can never mistake a note for a recording. */
    const val FAILURE_SUFFIX = ".failure"
    private const val DIR = "meetings"
    private const val OUTBOX = "outbox"
    private const val UPLOADED = "uploaded"
    private const val UNVERIFIED = "unverified"

    // How much shorter recall's copy may be before it stops counting as the same
    // recording. Two container probes of one file disagree by tens of milliseconds
    // (ffprobe on the server, MediaMetadataRetriever on the phone); a post that was cut
    // short loses seconds at least. A second and a half sits clear of the first and well
    // under the second.
    private const val LENGTH_TOLERANCE_MS = 1_500L

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

    /** Delivered, and recall's copy is as long as this one. */
    fun uploaded(ctx: Context): File = File(dir(ctx), UPLOADED).apply { mkdirs() }

    /** Delivered, but the lengths don't agree — look before deleting this one. */
    fun unverified(ctx: Context): File = File(dir(ctx), UNVERIFIED).apply { mkdirs() }

    /**
     * Whether recall's copy is materially shorter than the phone's, i.e. whether the 2xx
     * can be believed. Only *short* counts: a copy that probes a shade longer is two
     * decoders rounding the same file differently, not a loss.
     *
     * Unknown lengths (either probe failed) cannot be compared, and report `true` — the
     * whole point is to decide whether the upload has been *verified*, and an unanswered
     * question has not been.
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
     * Move a recording into [target] — how every state change happens here. Returns it at
     * its new home, or null if the rename failed, in which case it stays where it was
     * rather than quietly falling out of the flow.
     *
     * A move is always the end of the last attempt — approved, or delivered — so the
     * failure note goes with it. Leaving one behind would put "not authorised" under a
     * recording that is now safely on recall.
     */
    fun moveTo(recording: PendingRecording, target: File): PendingRecording? {
        val moved = File(target, recording.audio.name)
        if (!recording.audio.renameTo(moved)) return null
        clearFailure(recording.audio)
        return recording.copy(audio = moved)
    }

    /** Delete a recording. Only ever on the user's say-so. */
    fun delete(recording: PendingRecording) {
        clearFailure(recording.audio)
        recording.audio.delete()
    }

    /**
     * Why the last attempt to deliver [audio] didn't land, written beside it.
     *
     * On disk rather than in memory because the uploader runs under WorkManager with the
     * app gone: a reason held in a field is collected before anyone opens the screen to
     * find out, which is the state this whole task is fixing.
     */
    fun noteFailure(audio: File, reason: String) {
        runCatching { failureFile(audio).writeText(reason) }
    }

    /** Forget the last failure — a delivery landed, or the recording moved on. */
    fun clearFailure(audio: File) {
        runCatching { failureFile(audio).delete() }
    }

    /** The noted reason, or null if the last attempt succeeded or none has been made. */
    fun failure(audio: File): String? =
        runCatching { failureFile(audio).takeIf { it.isFile }?.readText() }
            .getOrNull()
            ?.takeIf { it.isNotBlank() }

    private fun failureFile(audio: File) = File(audio.parentFile, audio.name + FAILURE_SUFFIX)

    /** Discard a recording that never got any audio (stopped instantly, or failed). */
    fun discard(audio: File) {
        audio.delete()
    }
}
