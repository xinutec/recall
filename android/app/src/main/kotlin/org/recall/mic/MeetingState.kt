package org.recall.mic

import android.util.Log
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import java.io.File
import java.time.Instant

/**
 * Live meeting-recorder state, published by [MeetingService] and the upload worker and
 * observed by the UI — the same in-process StateFlow arrangement as [MicState], and
 * deliberately separate from it: the two modes never run at once, so sharing one
 * "running" flag would only invite the screen to render the wrong mode's status.
 */
object MeetingState {
    private val _recording = MutableStateFlow(false)
    val recording: StateFlow<Boolean> = _recording.asStateFlow()

    /** When the current recording started, for the elapsed-time readout; null if idle. */
    private val _startedAt = MutableStateFlow<Instant?>(null)
    val startedAt: StateFlow<Instant?> = _startedAt.asStateFlow()

    private val _level = MutableStateFlow(0f)
    val level: StateFlow<Float> = _level.asStateFlow()

    /** The file being recorded into right now — excluded from the library, since it is
     * not a thing that can be played, uploaded or deleted yet. */
    private val _activeFile = MutableStateFlow<File?>(null)
    val activeFile: StateFlow<File?> = _activeFile.asStateFlow()

    /**
     * Every recording still on the phone: held ones awaiting a decision and approved ones
     * on their way out. Shown in the app because audio that exists in exactly one place
     * and is invisible is how a lost recording goes unnoticed for weeks.
     */
    private val _recordings = MutableStateFlow<List<RecordingRow>>(emptyList())
    val recordings: StateFlow<List<RecordingRow>> = _recordings.asStateFlow()

    /** How many approved recordings have yet to reach the host. */
    private val _pending = MutableStateFlow(0)
    val pending: StateFlow<Int> = _pending.asStateFlow()

    /** Last thing that went wrong, for the screen; cleared when a recording starts. */
    private val _error = MutableStateFlow<String?>(null)
    val error: StateFlow<String?> = _error.asStateFlow()

    fun setRecording(value: Boolean, startedAt: Instant? = null, file: File? = null) {
        if (value != _recording.value) Log.i(UI_LOG, "meeting recording=$value")
        _recording.value = value
        _startedAt.value = if (value) startedAt else null
        _activeFile.value = if (value) file else null
        if (!value) _level.value = 0f
    }

    fun setRecordings(value: List<RecordingRow>) {
        _recordings.value = value
        setPending(value.count { it.queued })
    }

    fun setLevel(value: Float) {
        _level.value = value
    }

    fun setPending(value: Int) {
        if (value != _pending.value) Log.i(UI_LOG, "meeting uploads pending=$value")
        _pending.value = value
    }

    fun setError(value: String?) {
        if (value != null) Log.w(UI_LOG, "meeting error: $value")
        _error.value = value
    }
}
