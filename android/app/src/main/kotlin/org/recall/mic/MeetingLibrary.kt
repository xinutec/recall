package org.recall.mic

import android.content.Context
import android.media.MediaMetadataRetriever
import android.util.Log
import java.io.File
import java.time.ZoneId

/**
 * One recording as the screen shows it: what it is, how long, how big, and whether it is
 * still awaiting a decision or already on its way to the host.
 */
data class RecordingRow(
    val recording: PendingRecording,
    val durationMs: Long,
    val sizeBytes: Long,
    val queued: Boolean,
) {
    val file: File get() = recording.audio
}

/**
 * The recordings on this phone, as the meeting screen sees them — both the held ones
 * awaiting a decision and the approved ones on their way out, in one list, because the
 * question "what audio is still only on this phone?" has one answer.
 *
 * Nothing here uploads or deletes on its own. A recording sits in `meetings/` until the
 * user plays it and decides; [approve] is the only thing that hands it to
 * [MeetingUpload], and [delete] is the only thing that removes it unheard.
 */
object MeetingLibrary {
    private const val TAG = "recall.meeting"

    /** Re-read both directories and publish. Touches the filesystem — call it off the
     * main thread. */
    fun refresh(ctx: Context) {
        val zone = ZoneId.systemDefault()
        // The in-progress recording lives in the same directory and is a perfectly valid
        // partial Ogg — but offering Play/Upload/Delete on the file being written to is
        // nonsense, so it is excluded until it is finished.
        val active = MeetingState.activeFile.value
        val held =
            MeetingQueue
                .list(MeetingQueue.dir(ctx), zone)
                .filter { it.audio != active }
                .map { row(it, queued = false) }
        val queued =
            MeetingQueue
                .list(
                    MeetingQueue.outbox(ctx),
                    zone,
                ).map { row(it, queued = true) }
        MeetingState.setRecordings(held + queued)
    }

    /** Hand a recording to the uploader. Until this, nothing leaves the phone. */
    fun approve(ctx: Context, row: RecordingRow) {
        if (row.queued) return
        val moved = MeetingQueue.approve(row.recording, MeetingQueue.outbox(ctx))
        if (moved == null) {
            MeetingState.setError("Couldn't queue that recording — it is still on the phone.")
            Log.w(TAG, "approve failed for ${row.file.name}")
        } else {
            Log.i(UI_LOG, "meeting approved for upload: ${row.file.name}")
            MeetingUpload.enqueue(ctx)
        }
        refresh(ctx)
    }

    /** Delete a recording from the phone. There is no copy anywhere else yet. */
    fun delete(ctx: Context, row: RecordingRow) {
        if (MeetingPlayer.playingFile() == row.file) MeetingPlayer.stop()
        Log.i(UI_LOG, "meeting deleted from phone: ${row.file.name}")
        MeetingQueue.complete(row.recording)
        refresh(ctx)
    }

    private fun row(recording: PendingRecording, queued: Boolean) =
        RecordingRow(
            recording = recording,
            durationMs = durationMs(recording.audio),
            sizeBytes = recording.audio.length(),
            queued = queued,
        )

    /**
     * The recording's length, from the container. 0 when it can't be read — which is the
     * honest answer for a file cut short by a crash, and no reason to hide the row: a
     * truncated Ogg still plays and still uploads.
     */
    private fun durationMs(file: File): Long {
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
