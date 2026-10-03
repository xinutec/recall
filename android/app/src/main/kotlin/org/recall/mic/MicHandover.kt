package org.recall.mic

import java.util.concurrent.atomic.AtomicBoolean

/**
 * Whether continuous streaming currently holds the microphone, so a deliberate
 * meeting recording can wait for the handover instead of racing it.
 *
 * ⚠ `stopService` is asynchronous: `onDestroy` clears `running`, and the capture
 * thread releases its `AudioRecord` only when its next pass reaches the `finally`.
 * A `MediaRecorder` opened straight after `StreamService.stop` races that release;
 * both audio sources fail, and the user is sent to check permissions for a race in
 * our own code.
 *
 * ⚠ A paused stream never shows it: it releases the record on every failed connect
 * and sleeps between retries. Testing the handover needs capture actively running.
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
     * caller should still try, and say something true if it fails.
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
