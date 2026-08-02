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
import java.time.ZoneId
import java.util.concurrent.TimeUnit

/**
 * Drains [MeetingQueue] to the recall host, and keeps draining after the app is gone.
 *
 * A meeting is recorded where the host usually isn't reachable, so the upload can't be
 * part of pressing Stop. This is a `WorkManager` job instead: it survives the process, the
 * screen going off and a reboot, and retries with backoff until the host answers. Each run
 * uploads *everything* pending rather than one item, so a missed enqueue can't strand a
 * recording — the queue on disk is the state, not the job.
 */
class MeetingUpload(
    ctx: Context,
    params: WorkerParameters,
) : CoroutineWorker(ctx, params) {
    override suspend fun doWork(): Result {
        val host = Prefs.controlHost(applicationContext)
        val dir = MeetingQueue.dir(applicationContext)
        val queue = MeetingQueue.pending(dir, ZoneId.systemDefault())
        MeetingState.setPending(queue.size)
        if (queue.isEmpty()) return Result.success()

        var stuck = false
        for (recording in queue) {
            ShareUpload
                .upload(
                    host,
                    recording.audio,
                    recording.audio.name,
                    recording.start,
                    recording.title,
                ).onSuccess {
                    Log.i(UI_LOG, "meeting uploaded: ${recording.audio.name} -> $it")
                    // The host archive is what gets backed up, and this queue exists only
                    // to reach it — so once it's there, the phone's copy goes.
                    MeetingQueue.complete(recording)
                }.onFailure {
                    Log.w(UI_LOG, "meeting upload failed: ${recording.audio.name}: ${it.message}")
                    stuck = true
                }
            MeetingState.setPending(MeetingQueue.pending(dir, ZoneId.systemDefault()).size)
        }
        // Retry rather than fail: the usual reason is "not home yet", which time fixes.
        return if (stuck) Result.retry() else Result.success()
    }

    companion object {
        private const val WORK_NAME = "meeting-upload"

        /**
         * Try the queue now, and keep trying. REPLACE, not KEEP: every caller is an event
         * that means "the host might be reachable now" — the app opening, a recording
         * finishing, the mic stream connecting — so the backoff a previous failure earned
         * should be abandoned rather than waited out.
         */
        fun enqueue(ctx: Context) {
            // An empty queue is the common case — the mic stream calls this on every
            // reconnect — and waking WorkManager to discover that is pure cost.
            if (refreshPending(ctx) == 0) return
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

        /** Publish the queue depth without uploading — so the screen is honest on open —
         * and return it. */
        fun refreshPending(ctx: Context): Int {
            val depth = MeetingQueue.pending(MeetingQueue.dir(ctx), ZoneId.systemDefault()).size
            MeetingState.setPending(depth)
            return depth
        }

        private const val BACKOFF_S = 30L
    }
}
