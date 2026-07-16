package org.recall.mic

import android.util.Log
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow

// Shared tag for UI-state-change logging; filter with `adb logcat -s recall-ui:I`.
const val UI_LOG = "recall-ui"

/**
 * Live streaming state published by [StreamService] and observed by the UI. Same
 * process, so this is just shared in-memory state (StateFlow is thread-safe) — no
 * binding or IPC. The service writes from its capture thread; the UI collects it.
 */
object MicState {
    private val _running = MutableStateFlow(false)
    val running: StateFlow<Boolean> = _running.asStateFlow()

    private val _connected = MutableStateFlow(false)
    val connected: StateFlow<Boolean> = _connected.asStateFlow()

    /** Most recent mic peak amplitude, 0f..1f, for the level meter. */
    private val _level = MutableStateFlow(0f)
    val level: StateFlow<Float> = _level.asStateFlow()

    /**
     * The household capture (pause) state from /api/capture — the single value the
     * screen and the notification both render, so they can't disagree. Written by
     * whichever component is alive to poll (the open screen and/or the running
     * service); null until first read / when unreachable.
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
