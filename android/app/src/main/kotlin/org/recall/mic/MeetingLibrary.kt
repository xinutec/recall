package org.recall.mic

import android.content.Context
import android.media.MediaMetadataRetriever
import android.util.Log
import java.io.File
import java.time.ZoneId

/**
 * Where a recording has got to. Declared in the order the list shows them: what needs a
 * decision first, then what is in flight, then what needs a second look, then what is
 * settled.
 */
enum class RecordingState {
    /** On the phone and nowhere else. Nothing will send it until told. */
    HELD,

    /** Approved; waiting for the host to answer. */
    QUEUED,

    /** recall has it, but its copy is shorter than this one — or couldn't be compared. */
    UNVERIFIED,

    /** recall has it, and its copy is the same length. Safe to delete when you like. */
    UPLOADED,
}

/**
 * One recording as the screen shows it: what it is, how long, how big, and where it has
 * got to.
 */
data class RecordingRow(
    val recording: PendingRecording,
    val durationMs: Long,
    val sizeBytes: Long,
    val state: RecordingState,
    /** Why the last delivery attempt didn't land, or null if none has failed. Only ever
     * set on a [RecordingState.QUEUED] row — anywhere else it has been delivered. */
    val failure: String? = null,
) {
    val file: File get() = recording.audio
}

/**
 * The recordings on this phone, as the meeting screen sees them — held, in flight and
 * delivered, in one list, because the question "what audio is on this phone?" has one
 * answer, and the one about "which of it is safe to delete" is answered per row.
 *
 * Nothing here deletes on its own. [approve] hands a recording to [MeetingUpload]; a
 * successful upload files it under `uploaded/` or `unverified/` but leaves the audio
 * alone. [delete] is the only thing that removes anything, and only a person calls it.
 */
object MeetingLibrary {
    private const val TAG = "recall.meeting"

    /** Re-read the directories and publish. Touches the filesystem and probes every file
     * for its length — call it off the main thread. */
    fun refresh(ctx: Context) {
        val zone = ZoneId.systemDefault()
        // The in-progress recording lives in the same directory and is a perfectly valid
        // partial Ogg — but offering Play/Upload/Delete on the file being written to is
        // nonsense, so it is excluded until it is finished.
        val active = MeetingState.activeFile.value
        val rows =
            listOf(
                MeetingQueue.dir(ctx) to RecordingState.HELD,
                MeetingQueue.outbox(ctx) to RecordingState.QUEUED,
                MeetingQueue.unverified(ctx) to RecordingState.UNVERIFIED,
                MeetingQueue.uploaded(ctx) to RecordingState.UPLOADED,
            ).flatMap { (dir, state) ->
                MeetingQueue
                    .list(dir, zone)
                    .filter { it.audio != active }
                    .map { row(it, state) }
            }
        MeetingState.setRecordings(rows)
    }

    /** Hand a recording to the uploader. Until this, nothing leaves the phone. */
    fun approve(ctx: Context, row: RecordingRow) {
        if (row.state != RecordingState.HELD) return
        if (MeetingQueue.moveTo(row.recording, MeetingQueue.outbox(ctx)) == null) {
            MeetingState.setError("Couldn't queue that recording — it is still on the phone.")
            Log.w(TAG, "approve failed for ${row.file.name}")
        } else {
            Log.i(UI_LOG, "meeting approved for upload: ${row.file.name}")
            MeetingUpload.enqueue(ctx)
        }
        refresh(ctx)
    }

    /** Delete a recording from the phone. The only thing that ever removes one. */
    fun delete(ctx: Context, row: RecordingRow) {
        if (MeetingPlayer.playingFile() == row.file) MeetingPlayer.stop()
        Log.i(UI_LOG, "meeting deleted from phone: ${row.file.name} (was ${row.state})")
        MeetingQueue.delete(row.recording)
        refresh(ctx)
    }

    private fun row(recording: PendingRecording, state: RecordingState) =
        RecordingRow(
            recording = recording,
            durationMs = durationMs(recording.audio),
            sizeBytes = recording.audio.length(),
            state = state,
            failure =
                if (state == RecordingState.QUEUED) {
                    MeetingQueue.failure(recording.audio)
                } else {
                    null
                },
        )

    /**
     * The recording's length, from the container. 0 when it can't be read — which is the
     * honest answer for a file cut short by a crash, and no reason to hide the row: a
     * truncated Ogg still plays and still uploads. It is also what the upload check
     * compares against, where 0 means "couldn't verify", not "matches".
     */
    fun durationMs(file: File): Long {
        // Not `use`: MediaMetadataRetriever only became AutoCloseable in API 29, and this
        // has to work on everything the app installs on.
        val probe = MediaMetadataRetriever()
        return try {
            probe.setDataSource(file.path)
            probe.extractMetadata(MediaMetadataRetriever.METADATA_KEY_DURATION)?.toLongOrNull()
                ?: 0L
        } catch (e: RuntimeException) {
            Log.w(TAG, "could not read the length of ${file.name}: ${e.message}")
            0L
        } finally {
            runCatching { probe.release() }
        }
    }
}
