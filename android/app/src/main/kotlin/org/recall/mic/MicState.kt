package org.recall.mic

import android.util.Log
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update

// The tag for UI state changes: `adb logcat -s recall-ui:I`.
const val UI_LOG = "recall-ui"

/** The mic failed to open, as opposed to the network. */
internal class MicUnavailableException(
    message: String,
) : Exception(message)

/**
 * `micOk` after a failed streaming attempt. Only a mic failure changes it: other
 * failures happen before the mic opens (the socket connects first), so they say
 * nothing about it. `true` is written only where the mic opened.
 */
internal fun micOkAfter(previous: Boolean, failure: Throwable): Boolean =
    if (failure is MicUnavailableException) false else previous

/**
 * Streaming state, written by [StreamService] and read by the UI: one process, so
 * StateFlows suffice.
 */
object MicState {
    private val _running = MutableStateFlow(false)
    val running: StateFlow<Boolean> = _running.asStateFlow()

    private val _connected = MutableStateFlow(false)
    val connected: StateFlow<Boolean> = _connected.asStateFlow()

    /**
     * False while the mic will not open (permission revoked, held by another app).
     * Sent in the heartbeat (#887). Starts true: not known to be broken.
     */
    private val _micOk = MutableStateFlow(true)
    val micOk: StateFlow<Boolean> = _micOk.asStateFlow()

    fun setMicOk(value: Boolean) {
        _micOk.value = value
    }

    /**
     * Bytes of audio dropped since the app started because the stream's spool overran
     * ([PcmSpool]); normally zero. The heartbeat reports it.
     */
    private val _droppedBytes = MutableStateFlow(0L)
    val droppedBytes: StateFlow<Long> = _droppedBytes.asStateFlow()

    fun addDroppedBytes(delta: Long) {
        _droppedBytes.update { it + delta }
    }

    /** Most recent mic peak amplitude, 0f..1f, for the level meter. */
    private val _level = MutableStateFlow(0f)
    val level: StateFlow<Float> = _level.asStateFlow()

    /**
     * The household capture state from /api/capture, shown by both the screen and the
     * notification. Written by whichever is polling; null until first read.
     */
    private val _capture = MutableStateFlow<CaptureState?>(null)
    val capture: StateFlow<CaptureState?> = _capture.asStateFlow()

    fun setCapture(value: CaptureState?) {
        val old = _capture.value
        if (value?.running != old?.running || value?.settled != old?.settled) {
            Log.i(
                UI_LOG,
                "household capture running=${value?.running} " +
                    "desired=${value?.desiredRunning} settled=${value?.settled}",
            )
        }
        _capture.value = value
    }

    fun setRunning(value: Boolean) {
        if (value != _running.value) Log.i(UI_LOG, "service running=$value")
        _running.value = value
    }

    fun setConnected(value: Boolean) {
        if (value != _connected.value) Log.i(UI_LOG, "stream connected=$value (drives 'Streaming')")
        _connected.value = value
        if (!value) _level.value = 0f
    }

    fun setLevel(value: Float) {
        _level.value = value
    }
}
