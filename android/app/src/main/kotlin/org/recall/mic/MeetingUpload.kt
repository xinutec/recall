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
 * Sends every approved recording in the outbox, and only those. The files are the state,
 * so a missed enqueue strands nothing. A `WorkManager` job, so it survives the app and a
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
            // Reported too, so the server's check can return to green.
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
                    // Beside the recording, for the screen to show.
                    MeetingQueue.noteFailure(recording.audio, UploadFailure.describe(it))
                    stuck = true
                }
            MeetingLibrary.refresh(applicationContext)
        }
        // What is left after the pass.
        report(host, token, outbox)
        // Usually "not home yet", which time fixes.
        return if (stuck) Result.retry() else Result.success()
    }

    /** Report what is still here (#77); best-effort, never failing the pass. */
    private suspend fun report(host: String, token: String, outbox: File) {
        OutboxReport.send(
            host,
            Prefs.deviceId(applicationContext),
            MeetingQueue.state(outbox, ZoneId.systemDefault()),
            token,
        )
    }

    /**
     * File a delivered recording under `uploaded/` or, if recall's copy is shorter (a post
     * cut short still gets a 2xx), `unverified/`. The phone's copy stays either way.
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
        // The numbers too, to explain an "unverified" later.
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
         * Try the outbox now, and keep trying. REPLACE: every caller (an approval, the
         * screen opening, the stream connecting) means the host may be reachable now, so
         * an earlier failure's backoff is dropped.
         */
        fun enqueue(ctx: Context, always: Boolean = false) {
            // An empty outbox is common (the stream calls this on every reconnect), so it
            // is skipped, unless the caller emptied it (`always`): the pass then runs for
            // its report, or the server would go on reading "failing".
            if (!always && approvedCount(ctx) == 0) return
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

        private fun approvedCount(ctx: Context): Int =
            MeetingQueue.list(MeetingQueue.outbox(ctx), ZoneId.systemDefault()).size

        private const val BACKOFF_S = 30L
    }
}
