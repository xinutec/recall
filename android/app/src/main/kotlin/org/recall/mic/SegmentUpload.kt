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
import org.json.JSONObject
import java.io.File
import java.net.HttpURLConnection
import java.net.URL
import java.security.MessageDigest
import java.time.Instant
import java.util.concurrent.TimeUnit

/**
 * Delivers closed segments to recalld's ingest, oldest first. A delivery counts
 * only when the receipt's sha-256 and length match the bytes sent
 * (docs/architecture.md, decision 3); then the segment moves to `delivered/`. A
 * 409 or a refusal moves it to `conflict/` for a person to look at; anything else
 * waits for the next pass.
 *
 * Unmetered networks only; the cache holds the audio meanwhile.
 */
class SegmentUpload(
    ctx: Context,
    params: WorkerParameters,
) : CoroutineWorker(ctx, params) {
    override suspend fun doWork(): Result {
        val ctx = applicationContext
        val base =
            ctx.getExternalFilesDir(android.os.Environment.DIRECTORY_MUSIC) ?: return Result.retry()
        val url = Prefs.ingestBase(ctx)
        val token = Prefs.ingestToken(ctx)
        var stuck = false
        for (segment in SegmentStore.undelivered(base)) {
            when (val verdict = deliver(url, token, Prefs.deviceId(ctx), segment)) {
                Delivery.VERIFIED -> {
                    SegmentStore.markDelivered(base, segment)
                }

                Delivery.CONFLICT -> {
                    Log.w(TAG, "segment conflict (name held by different bytes): ${segment.name}")
                    SegmentStore.markConflict(base, segment)
                }

                Delivery.FAILED -> {
                    Log.w(TAG, "segment upload failed, will retry: ${segment.name}")
                    stuck = true
                    // Stop the pass; the backoff decides when to try again.
                    break
                }

                Delivery.REJECTED -> {
                    // Retrying cannot help (bad name, oversize); kept for a person.
                    Log.w(TAG, "segment rejected ($verdict): ${segment.name}")
                    SegmentStore.markConflict(base, segment)
                }
            }
        }
        SegmentStore.evict(base)
        return if (stuck) Result.retry() else Result.success()
    }

    enum class Delivery { VERIFIED, CONFLICT, REJECTED, FAILED }

    private fun deliver(base: String, token: String, source: String, segment: File): Delivery =
        try {
            val bytes = segment.readBytes()
            val sha = sha256Hex(bytes)
            val conn =
                URL("$base/ingest/v1/segments/$source/${segment.name}")
                    .openConnection() as HttpURLConnection
            try {
                conn.requestMethod = "PUT"
                conn.doOutput = true
                conn.connectTimeout = CONNECT_TIMEOUT_MS
                conn.readTimeout = READ_TIMEOUT_MS
                conn.setFixedLengthStreamingMode(bytes.size)
                conn.setRequestProperty("Content-Type", "application/octet-stream")
                conn.setRequestProperty("X-Recall-Sent", Instant.now().toString())
                if (token.isNotEmpty()) {
                    conn.setRequestProperty("Authorization", "Bearer $token")
                }
                conn.outputStream.use { it.write(bytes) }
                when (val code = conn.responseCode) {
                    200 -> {
                        if (receiptMatches(
                                conn.inputStream.readBytes().decodeToString(),
                                sha,
                                bytes.size,
                            )
                        ) {
                            Delivery.VERIFIED
                        } else {
                            Delivery.FAILED
                        }
                    }

                    409 -> {
                        Delivery.CONFLICT
                    }

                    // A missing or wrong token, not a bad segment: retried, so
                    // the backlog drains once the token is set.
                    401, 403 -> {
                        Delivery.FAILED
                    }

                    in 400..499 -> {
                        Delivery.REJECTED
                    }

                    else -> {
                        Log.w(TAG, "ingest answered $code for ${segment.name}")
                        Delivery.FAILED
                    }
                }
            } finally {
                conn.disconnect()
            }
        } catch (e: Exception) {
            Log.w(TAG, "ingest unreachable: ${e.message}")
            Delivery.FAILED
        }

    companion object {
        private const val TAG = "recall-mic"
        private const val WORK = "segment-upload"
        private const val CONNECT_TIMEOUT_MS = 10_000
        private const val READ_TIMEOUT_MS = 60_000

        /** Whether the receipt names our hash and byte count. */
        fun receiptMatches(body: String, sha256: String, bytes: Int): Boolean =
            runCatching {
                val receipt = JSONObject(body)
                receipt.getString("sha256") == sha256 && receipt.getInt("bytes") == bytes
            }.getOrDefault(false)

        fun sha256Hex(bytes: ByteArray): String =
            MessageDigest.getInstance("SHA-256").digest(bytes).joinToString("") { b ->
                "%02x".format(b)
            }

        /** Drain now; REPLACE drops an earlier failure's backoff. */
        fun enqueue(ctx: Context) {
            val request =
                OneTimeWorkRequestBuilder<SegmentUpload>()
                    .setConstraints(
                        Constraints.Builder().setRequiredNetworkType(NetworkType.UNMETERED).build(),
                    ).setBackoffCriteria(BackoffPolicy.EXPONENTIAL, 1, TimeUnit.MINUTES)
                    .build()
            WorkManager
                .getInstance(
                    ctx,
                ).enqueueUniqueWork(WORK, ExistingWorkPolicy.REPLACE, request)
        }
    }
}
