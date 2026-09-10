package org.recall.mic

import java.util.concurrent.atomic.AtomicBoolean

/**
 * Whether continuous streaming currently holds the microphone, so a deliberate
 * meeting recording can wait for the handover instead of racing it.
 *
 * ⚠ **The race this exists to remove.** `MeetingService.beginOnQ` called
 * `StreamService.stop(this)` and then IMMEDIATELY opened a `MediaRecorder`.
 * `stopService` is asynchronous: `onDestroy` clears `running`, and only on its
 * NEXT iteration does the capture thread reach the `finally` block that runs
 * `record.stop()` / `record.release()`. While continuous capture was running
 * there was therefore a window in which the old `AudioRecord` was still open —
 * both audio sources failed and the user was told to check permissions, which
 * sent them into Android settings looking for a race in our own handover.
 *
 * ⚠ **Why nobody hit it while the household was paused:** a paused stream has
 * already released the record in that same `finally` on every failed connect
 * cycle, and then sleeps between retries. A paused house leaves the mic free, so
 * a meeting started during a pause acquires it cleanly. The bug needs capture to
 * be actively running.
 */
object MicHandover {
    private val held = AtomicBoolean(false)

    /** The streaming capture has an `AudioRecord` open. */
    fun acquired() = held.set(true)

    /** The streaming capture has stopped and released it. */
    fun released() = held.set(false)

    fun isHeld(): Boolean = held.get()

    /**
     * Wait up to [timeoutMs] for the streaming capture to let go. `true` if the
     * microphone is free.
     *
     * ⚠ **Bounded on purpose.** A capture thread wedged on a dead socket would
     * otherwise hang the deliberate act indefinitely, and a meeting somebody has
     * pressed record on is worth more than a tidy handover — so on a timeout the
     * caller should still TRY, and say something true if it fails.
     */
    fun awaitRelease(timeoutMs: Long): Boolean {
        val deadline = System.nanoTime() + timeoutMs * 1_000_000
        while (held.get()) {
            if (System.nanoTime() >= deadline) return false
            Thread.sleep(POLL_MS)
        }
        return true
    }

    /**
     * What to tell somebody whose recording would not start.
     *
     * ⚠ The message is half the bug. Blaming permissions when we know our own
     * stream still held the microphone sends the reader somewhere the fault has
     * never been.
     */
    fun failureMessage(handedOver: Boolean): String =
        if (handedOver) {
            "Microphone unavailable — check permission / other apps."
        } else {
            "Microphone still busy: streaming did not let go in time. Try again."
        }

    private const val POLL_MS = 10L

    /** How long a deliberate recording waits for the stream to release. */
    const val HANDOVER_MS = 2_000L
}
