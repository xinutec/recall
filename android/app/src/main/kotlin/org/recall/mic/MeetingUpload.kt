package org.recall.mic

import android.content.Context
import android.util.Log
import androidx.work.BackoffPolicy
import androidx.work.Constraints
import androidx.work.CoroutineWorker
import androidx.work.ExistingWorkPolicy
import androidx.work.NetworkType
import androidx.work.OneTimeWorkRequestBuilder
import androidx.work.WorkManager
import androidx.work.WorkerParameters
import java.io.File
import java.time.ZoneId
import java.util.concurrent.TimeUnit

/**
 * Sends the recordings the user approved, and keeps trying after the app is gone.
 *
 * It drains **the outbox only** — nothing is uploaded because it was recorded, only
 * because it was approved. Within the outbox it takes everything rather than one named
 * item, so a missed enqueue can't strand an approved recording: the files on disk are the
 * state, not the job. A meeting is recorded where the host usually isn't reachable, so
 * this is a `WorkManager` job that survives the process, the screen going off and a
 * reboot, retrying with backoff until the host answers.
 */
class MeetingUpload(
    ctx: Context,
    params: WorkerParameters,
) : CoroutineWorker(ctx, params) {
    override suspend fun doWork(): Result {
        val host = Prefs.controlHost(applicationContext)
        val token = Prefs.deviceToken(applicationContext)
        val outbox = MeetingQueue.outbox(applicationContext)
        val queue = MeetingQueue.list(outbox, ZoneId.systemDefault())
        if (queue.isEmpty()) {
            MeetingLibrary.refresh(applicationContext)
            // Still report: an empty outbox is the reading that lets the fleet check
            // go back to green, and one that cannot is one that gets muted.
            report(host, token, outbox)
            return Result.success()
        }

        var stuck = false
        for (recording in queue) {
            ShareUpload
                .upload(host, recording.audio, recording.audio.name, recording.start, token)
                .onSuccess { file(recording, it) }
                .onFailure {
                    Log.w(UI_LOG, "meeting upload failed: ${recording.audio.name}: ${it.message}")
                    // Beside the recording, not only in the log: the screen is where
                    // somebody asks why, and this worker runs with the app gone.
                    MeetingQueue.noteFailure(recording.audio, UploadFailure.describe(it))
                    stuck = true
                }
            MeetingLibrary.refresh(applicationContext)
        }
        // After the pass, not before: the fleet wants what is left, not what was there.
        report(host, token, outbox)
        // Retry rather than fail: the usual reason is "not home yet", which time fixes.
        return if (stuck) Result.retry() else Result.success()
    }

    /**
     * Say what is still here. Best-effort and unchecked — a report about undelivered
     * recordings that failed the pass because the report failed would be absurd, and
     * the fleet reads a missing report as a finding of its own (#77).
     */
    private suspend fun report(host: String, token: String, outbox: File) {
        OutboxReport.send(
            host,
            Prefs.deviceId(applicationContext),
            MeetingQueue.state(outbox, ZoneId.systemDefault()),
            token,
        )
    }

    /**
     * Put a delivered recording where its verdict says it belongs. A 2xx only means recall
     * received *something*: the server probes what arrived, so a post cut short mid-stream
     * still parses and still succeeds — with seconds missing off the end. Comparing the
     * length recall reports against the file still on the phone is what turns "sent" into
     * "sent intact", and the phone's copy is kept either way.
     */
    private fun file(recording: PendingRecording, session: UploadedSession) {
        val localMs = MeetingLibrary.durationMs(recording.audio)
        val short = MeetingQueue.landedShort(localMs, session.durationMs)
        val target =
            if (short) {
                MeetingQueue.unverified(applicationContext)
            } else {
                MeetingQueue.uploaded(applicationContext)
            }
        // The numbers, not just the verdict: "why does it say unverified" has to be
        // answerable from the log after the fact.
        Log.i(
            UI_LOG,
            "meeting uploaded: ${recording.audio.name} -> ${session.title} " +
                "(phone ${localMs}ms, recall ${session.durationMs}ms" +
                if (short) ", NOT VERIFIED)" else ")",
        )
        MeetingQueue.moveTo(recording, target)
    }

    companion object {
        private const val WORK_NAME = "meeting-upload"

        /**
         * Try the outbox now, and keep trying. REPLACE, not KEEP: every caller is an event
         * that means "the host might be reachable now" — a recording being approved, the
         * screen opening, the mic stream connecting — so the backoff a previous failure
         * earned should be abandoned rather than waited out.
         */
        fun enqueue(ctx: Context) {
            // An empty outbox is the common case — the mic stream calls this on every
            // reconnect — and waking WorkManager to discover that is pure cost.
            if (approvedCount(ctx) == 0) return
            WorkManager.getInstance(ctx).enqueueUniqueWork(
                WORK_NAME,
                ExistingWorkPolicy.REPLACE,
                OneTimeWorkRequestBuilder<MeetingUpload>()
                    .setConstraints(
                        Constraints
                            .Builder()
                            .setRequiredNetworkType(NetworkType.CONNECTED)
                            .build(),
                    ).setBackoffCriteria(BackoffPolicy.EXPONENTIAL, BACKOFF_S, TimeUnit.SECONDS)
                    .build(),
            )
        }

        /** How many recordings are approved and not yet delivered. */
        private fun approvedCount(ctx: Context): Int =
            MeetingQueue.list(MeetingQueue.outbox(ctx), ZoneId.systemDefault()).size

        private const val BACKOFF_S = 30L
    }
}
