package org.recall.mic

import android.content.Context
import android.media.MediaMetadataRetriever
import android.util.Log
import java.io.File
import java.time.ZoneId

/** Where a recording has got to, in the order the list shows them. */
enum class RecordingState {
    /** On the phone only; not sent until approved. */
    HELD,

    /** Approved; waiting for the host. */
    QUEUED,

    /** recall's copy is shorter, or could not be compared. */
    UNVERIFIED,

    /** recall's copy is the same length; safe to delete. */
    UPLOADED,
}

/** One recording as the meeting screen shows it. */
data class RecordingRow(
    val recording: PendingRecording,
    val durationMs: Long,
    val sizeBytes: Long,
    val state: RecordingState,
    /** Why the last delivery failed; only on a [RecordingState.QUEUED] row. */
    val failure: String? = null,
) {
    val file: File get() = recording.audio
}

/**
 * Every recording on the phone, in one list for the meeting screen. [approve] hands one
 * to [MeetingUpload]; [delete], called only by a person, is the only removal.
 */
object MeetingLibrary {
    private const val TAG = "recall.meeting"

    /** Re-read the directories and publish. Probes every file; off the main thread. */
    fun refresh(ctx: Context) {
        val zone = ZoneId.systemDefault()
        // Not the file still being recorded.
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

    /** Hand a recording to the uploader. */
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

    /** Delete a recording from the phone. */
    fun delete(ctx: Context, row: RecordingRow) {
        if (MeetingPlayer.playingFile() == row.file) MeetingPlayer.stop()
        Log.i(UI_LOG, "meeting deleted from phone: ${row.file.name} (was ${row.state})")
        val wasQueued = row.state == RecordingState.QUEUED
        MeetingQueue.delete(row.recording)
        refresh(ctx)
        // A pass, so the server hears the outbox changed.
        if (wasQueued) MeetingUpload.enqueue(ctx, always = true)
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
     * The recording's length, from the container; 0 if unreadable, as after a crash
     * (the file still plays and uploads). The upload check reads 0 as unverified.
     */
    fun durationMs(file: File): Long {
        // Not `use`: AutoCloseable only from API 29.
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
