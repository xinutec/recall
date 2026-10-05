package org.recall.mic

import android.media.AudioAttributes
import android.media.MediaPlayer
import android.util.Log
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import java.io.File

/**
 * Plays a recording back, to check it before uploading: one file at a time, no service.
 * The screen polls the position. The device volume is left alone.
 */
object MeetingPlayer {
    private const val TAG = "recall.meeting"

    // dev-lint: allow-object-var the one MediaPlayer this process owns; no test reaches it
    private var player: MediaPlayer? = null

    /** The loaded file, if any. */
    private val _file = MutableStateFlow<File?>(null)
    val file: StateFlow<File?> = _file.asStateFlow()

    private val _playing = MutableStateFlow(false)
    val playing: StateFlow<Boolean> = _playing.asStateFlow()

    private val _durationMs = MutableStateFlow(0L)
    val durationMs: StateFlow<Long> = _durationMs.asStateFlow()

    fun playingFile(): File? = _file.value

    /** Play [target], or pause/resume it if it is already the loaded one. */
    fun toggle(target: File) {
        if (_file.value == target) {
            player?.let { if (it.isPlaying) pause() else resume() } ?: load(target)
            return
        }
        load(target)
    }

    fun pause() {
        runCatching { player?.pause() }
        _playing.value = false
    }

    fun resume() {
        runCatching { player?.start() }
        _playing.value = player?.isPlaying == true
    }

    fun seekTo(ms: Long) {
        runCatching { player?.seekTo(ms.toInt()) }
    }

    /** The playback position, in ms. */
    fun positionMs(): Long =
        runCatching { player?.currentPosition?.toLong() ?: 0L }.getOrDefault(0L)

    fun stop() {
        runCatching { player?.release() }
        player = null
        _file.value = null
        _playing.value = false
        _durationMs.value = 0L
    }

    /** [stop], named for the lifecycle callers. */
    fun release() = stop()

    private fun load(target: File) {
        stop()
        val mp = MediaPlayer()
        val ok =
            runCatching {
                mp.setAudioAttributes(
                    AudioAttributes
                        .Builder()
                        .setUsage(AudioAttributes.USAGE_MEDIA)
                        .setContentType(AudioAttributes.CONTENT_TYPE_SPEECH)
                        .build(),
                )
                mp.setDataSource(target.path)
                mp.prepare()
                mp.setOnCompletionListener { _playing.value = false }
                mp.start()
            }.isSuccess
        if (!ok) {
            Log.w(TAG, "could not play ${target.name}")
            runCatching { mp.release() }
            MeetingState.setError("That recording couldn't be played.")
            return
        }
        player = mp
        _file.value = target
        _durationMs.value = runCatching { mp.duration.toLong() }.getOrDefault(0L)
        _playing.value = true
    }
}
