package org.recall.mic

import java.util.concurrent.atomic.AtomicBoolean

/**
 * Whether the stream holds the microphone, so a meeting recording waits for it to let
 * go (#1472). `stopService` is asynchronous: the capture thread releases its
 * `AudioRecord` only on its next pass, and a `MediaRecorder` opened before then fails.
 *
 * A paused stream releases the mic between retries, so testing this needs capture
 * running.
 */
object MicHandover {
    private val held = AtomicBoolean(false)

    /** The streaming capture has an `AudioRecord` open. */
    fun acquired() = held.set(true)

    /** The streaming capture has stopped and released it. */
    fun released() = held.set(false)

    fun isHeld(): Boolean = held.get()

    /**
     * Wait up to [timeoutMs] for the stream to let go; `true` if the mic is free.
     * Bounded, since a capture thread can wedge on a dead socket; on a timeout the
     * caller still tries.
     */
    fun awaitRelease(timeoutMs: Long): Boolean {
        val deadline = System.nanoTime() + timeoutMs * 1_000_000
        while (held.get()) {
            if (System.nanoTime() >= deadline) return false
            Thread.sleep(POLL_MS)
        }
        return true
    }

    /** Why a recording would not start: permissions only if the stream had let go. */
    fun failureMessage(handedOver: Boolean): String =
        if (handedOver) {
            "Microphone unavailable — check permission / other apps."
        } else {
            "Microphone still busy: streaming did not let go in time. Try again."
        }

    private const val POLL_MS = 10L

    /** How long a meeting recording waits for the stream to let go. */
    const val HANDOVER_MS = 2_000L
}
