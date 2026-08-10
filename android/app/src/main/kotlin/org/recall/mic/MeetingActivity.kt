package org.recall.mic

import android.Manifest
import android.content.pm.PackageManager
import android.os.Build
import android.os.Bundle
import android.util.Log
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Slider
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import androidx.core.content.ContextCompat
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.lifecycleScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import java.time.Instant
import java.time.ZoneId

/**
 * The meeting-recorder screen: the recorder itself, then everything still on the phone —
 * each one playable, deletable, and uploadable *only* when the user says so.
 *
 * Nothing leaves the phone on its own. A finished recording sits in the list until it is
 * listened to and a decision is made about it; that is the whole point of the split
 * between `meetings/` and `meetings/outbox/` in [MeetingQueue].
 *
 * The recording belongs to [MeetingService], which outlives this screen — an appointment
 * is recorded with the phone in a pocket and the screen off. Playback, by contrast, is
 * tied to this screen and released when it goes away.
 */
class MeetingActivity : ComponentActivity() {
    private val requestPermissions =
        registerForActivityResult(ActivityResultContracts.RequestMultiplePermissions()) { grants ->
            if (grants[Manifest.permission.RECORD_AUDIO] == true) {
                MeetingService.start(this)
            } else {
                MeetingState.setError("Recording needs microphone permission.")
            }
        }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        enableEdgeToEdge()
        setContent {
            RecallMicTheme {
                MeetingScreen(
                    onStart = ::begin,
                    onStop = ::end,
                    onPlay = { MeetingPlayer.toggle(it.file) },
                    onUpload = ::approve,
                    onDelete = ::discard,
                    onRetry = { MeetingUpload.enqueue(this) },
                    onBack = { finish() },
                )
            }
        }
    }

    override fun onStart() {
        super.onStart()
        refresh()
        // Opening the screen is the clearest "am I home yet?" signal there is, so anything
        // already approved gets another go at the host.
        MeetingUpload.enqueue(this)
    }

    override fun onStop() {
        // Leaving the screen releases the codec; playback is a check, not a background
        // feature, and it must not outlive the list it belongs to.
        MeetingPlayer.release()
        super.onStop()
    }

    private fun begin() {
        Log.i(UI_LOG, "button: Record meeting")
        val needed =
            buildList {
                add(Manifest.permission.RECORD_AUDIO)
                if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
                    add(Manifest.permission.POST_NOTIFICATIONS)
                }
            }.filter {
                ContextCompat.checkSelfPermission(this, it) != PackageManager.PERMISSION_GRANTED
            }
        if (needed.isNotEmpty()) {
            requestPermissions.launch(needed.toTypedArray())
        } else {
            MeetingService.start(this)
        }
    }

    private fun end() {
        Log.i(UI_LOG, "button: Stop meeting")
        MeetingService.stop(this)
    }

    private fun approve(row: RecordingRow) {
        Log.i(UI_LOG, "button: Upload recording")
        lifecycleScope.launch {
            withContext(Dispatchers.IO) { MeetingLibrary.approve(this@MeetingActivity, row) }
        }
    }

    private fun discard(row: RecordingRow) {
        Log.i(UI_LOG, "button: Delete recording")
        lifecycleScope.launch {
            withContext(Dispatchers.IO) { MeetingLibrary.delete(this@MeetingActivity, row) }
        }
    }

    /** Re-read the directories off the main thread — it probes each file for its length. */
    private fun refresh() {
        lifecycleScope.launch {
            withContext(Dispatchers.IO) { MeetingLibrary.refresh(this@MeetingActivity) }
        }
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun MeetingScreen(
    onStart: () -> Unit,
    onStop: () -> Unit,
    onPlay: (RecordingRow) -> Unit,
    onUpload: (RecordingRow) -> Unit,
    onDelete: (RecordingRow) -> Unit,
    onRetry: () -> Unit,
    onBack: () -> Unit,
) {
    val recording by MeetingState.recording.collectAsStateWithLifecycle()
    val startedAt by MeetingState.startedAt.collectAsStateWithLifecycle()
    val level by MeetingState.level.collectAsStateWithLifecycle()
    val recordings by MeetingState.recordings.collectAsStateWithLifecycle()
    val pending by MeetingState.pending.collectAsStateWithLifecycle()
    val error by MeetingState.error.collectAsStateWithLifecycle()
    // Deleting is the one action with no undo — the phone holds the only copy.
    var confirmDelete by remember { mutableStateOf<RecordingRow?>(null) }

    // A ticking clock for the elapsed readout — a second, since it counts seconds.
    var now by remember { mutableStateOf(Instant.now()) }
    LaunchedEffect(recording) {
        while (recording) {
            now = Instant.now()
            delay(1_000)
        }
    }

    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text("Record a meeting") },
                navigationIcon = {
                    IconButton(onClick = onBack) {
                        Icon(painterResource(R.drawable.ic_back), contentDescription = "Back")
                    }
                },
            )
        },
    ) { inner ->
        Column(
            Modifier
                .padding(inner)
                .fillMaxSize()
                .verticalScroll(rememberScrollState())
                .padding(20.dp),
            verticalArrangement = Arrangement.spacedBy(16.dp),
        ) {
            RecorderCard(recording, startedAt, now, level)

            Button(
                onClick = { if (recording) onStop() else onStart() },
                modifier = Modifier.fillMaxWidth(),
            ) { Text(if (recording) "Stop and save" else "Start recording") }

            error?.let {
                Text(
                    it,
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.error,
                )
            }

            RecordingList(
                recordings = recordings,
                pending = pending,
                onPlay = onPlay,
                onUpload = onUpload,
                onDelete = { confirmDelete = it },
                onRetry = onRetry,
            )
        }
    }

    confirmDelete?.let { row ->
        AlertDialog(
            onDismissRequest = { confirmDelete = null },
            title = { Text("Delete this recording?") },
            text = {
                Text(
                    when (row.state) {
                        RecordingState.UPLOADED -> {
                            "recall has this one, and its copy is the same length. " +
                                "Deleting it here frees the space."
                        }

                        RecordingState.UNVERIFIED -> {
                            "recall received this, but its copy is SHORTER than the one " +
                                "here — this may be the only complete recording. " +
                                "Deleting it cannot be undone."
                        }

                        else -> {
                            "This phone has the only copy — it has not been uploaded. " +
                                "Deleting it cannot be undone."
                        }
                    },
                )
            },
            confirmButton = {
                TextButton(onClick = {
                    onDelete(row)
                    confirmDelete = null
                }) { Text("Delete") }
            },
            dismissButton = {
                TextButton(onClick = { confirmDelete = null }) { Text("Keep") }
            },
        )
    }
}

@Composable
private fun RecorderCard(recording: Boolean, startedAt: Instant?, now: Instant, level: Float) {
    Card(Modifier.fillMaxWidth()) {
        Column(
            Modifier.fillMaxWidth().padding(20.dp),
            verticalArrangement = Arrangement.spacedBy(16.dp),
        ) {
            Text(
                startedAt?.let { elapsedLabel(it, now) } ?: "00:00",
                style = MaterialTheme.typography.displayMedium,
                color =
                    if (recording) {
                        MaterialTheme.colorScheme.primary
                    } else {
                        MaterialTheme.colorScheme.onSurfaceVariant
                    },
                textAlign = TextAlign.Center,
                modifier = Modifier.fillMaxWidth(),
            )
            Text(
                if (recording) "Recording — keep the phone out of a pocket" else "Ready",
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                textAlign = TextAlign.Center,
                modifier = Modifier.fillMaxWidth(),
            )
            LevelMeter(level)
        }
    }
}

/** Everything still on the phone. Never hidden: audio that exists in one place and is
 * invisible is how a recording goes unnoticed for weeks. */
@Composable
private fun RecordingList(
    recordings: List<RecordingRow>,
    pending: Int,
    onPlay: (RecordingRow) -> Unit,
    onUpload: (RecordingRow) -> Unit,
    onDelete: (RecordingRow) -> Unit,
    onRetry: () -> Unit,
) {
    Text("On this phone", style = MaterialTheme.typography.titleMedium)
    if (recordings.isEmpty()) {
        Text(
            "Nothing recorded yet. Recordings stay on the phone until you delete them.",
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        return
    }
    Card(
        colors =
            CardDefaults.cardColors(containerColor = MaterialTheme.colorScheme.surfaceVariant),
        modifier = Modifier.fillMaxWidth(),
    ) {
        Column(Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
            recordings.forEachIndexed { index, row ->
                if (index > 0) HorizontalDivider()
                RecordingItem(row, onPlay, onUpload, onDelete)
            }
        }
    }
    if (pending > 0) {
        // "Waiting for the recall host" is the same untruth the rows used to tell, one
        // level up: it names the one cause — not home yet — while the row above it says
        // the token is wrong. Seen side by side on the real screen, the footer reads as
        // the summary and quietly contradicts the diagnosis.
        val failing = recordings.count { it.failure != null }
        Row(
            horizontalArrangement = Arrangement.spacedBy(12.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Text(
                if (failing == 0) {
                    "Waiting for the recall host…"
                } else {
                    "$failing of $pending couldn't be sent — see above."
                },
                style = MaterialTheme.typography.bodySmall,
                color =
                    if (failing == 0) {
                        MaterialTheme.colorScheme.onSurfaceVariant
                    } else {
                        MaterialTheme.colorScheme.error
                    },
            )
            OutlinedButton(onClick = onRetry) { Text("Try now") }
        }
    }
}

@Composable
private fun RecordingItem(
    row: RecordingRow,
    onPlay: (RecordingRow) -> Unit,
    onUpload: (RecordingRow) -> Unit,
    onDelete: (RecordingRow) -> Unit,
) {
    val playingFile by MeetingPlayer.file.collectAsStateWithLifecycle()
    val playing by MeetingPlayer.playing.collectAsStateWithLifecycle()
    val isThis = playingFile == row.file

    Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
        // When it was recorded is the whole identity of a recording — recall names the
        // session the same way, and it can be renamed there once there's a transcript to
        // name it after.
        Text(
            startedLabel(row.recording.start, ZoneId.systemDefault()),
            style = MaterialTheme.typography.titleSmall,
            fontWeight = FontWeight.Bold,
        )
        Text(
            "${elapsedLabel(row.durationMs / 1000)} · ${sizeLabel(row.sizeBytes)}",
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )

        row.failure?.let { why ->
            // Same place and the same colour as the length warning below, because it is
            // the same kind of thing: something about THIS recording that the person
            // deciding what to do with it needs, said where the buttons are rather than
            // in a log nobody reads.
            Text(
                why,
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.error,
            )
        }

        if (row.state == RecordingState.UNVERIFIED) {
            // The one case where the phone's copy is the better one — say so where the
            // Delete button is, not in a log nobody reads.
            Text(
                "recall received this, but its copy is shorter than the one here. " +
                    "Keep this until you have checked it.",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.error,
            )
        }

        if (isThis) PlaybackBar(row)

        Row(
            horizontalArrangement = Arrangement.spacedBy(8.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            OutlinedButton(onClick = { onPlay(row) }) {
                Text(if (isThis && playing) "Pause" else "Play")
            }
            when (row.state) {
                RecordingState.HELD -> {
                    Button(onClick = { onUpload(row) }) { Text("Upload") }
                }

                RecordingState.QUEUED -> {
                    // "Uploading…" next to a recording that has been failing for an hour
                    // is the lie this task exists to end: the count on the meeting screen
                    // read the same whether the host was unreachable or the token was
                    // wrong, so a 401 survived from the day the feature was written.
                    if (row.failure == null) {
                        StateNote("Uploading…", MaterialTheme.colorScheme.primary)
                    } else {
                        StateNote("Upload failed", MaterialTheme.colorScheme.error)
                    }
                }

                RecordingState.UPLOADED -> {
                    StateNote("On recall ✓", MaterialTheme.colorScheme.primary)
                }

                RecordingState.UNVERIFIED -> {
                    StateNote("Length doesn't match", MaterialTheme.colorScheme.error)
                }
            }
            TextButton(onClick = { onDelete(row) }) { Text("Delete") }
        }
    }
}

/** A row's state, where the Upload button would otherwise be. */
@Composable
private fun StateNote(text: String, color: Color) {
    Text(text, style = MaterialTheme.typography.bodySmall, color = color)
}

/** Position and a scrubber for the recording being played — an appointment is an hour
 * long, so listening back without seeking is no use. */
@Composable
private fun PlaybackBar(row: RecordingRow) {
    val playing by MeetingPlayer.playing.collectAsStateWithLifecycle()
    val duration by MeetingPlayer.durationMs.collectAsStateWithLifecycle()
    // Pulled, not pushed: nothing polls while this screen is closed.
    var position by remember { mutableStateOf(0L) }
    LaunchedEffect(playing, row.file) {
        while (true) {
            position = MeetingPlayer.positionMs()
            delay(250)
        }
    }
    val total = if (duration > 0) duration else row.durationMs
    Column {
        Slider(
            value = if (total > 0) (position.toFloat() / total).coerceIn(0f, 1f) else 0f,
            onValueChange = { MeetingPlayer.seekTo((it * total).toLong()) },
        )
        Text(
            "${elapsedLabel(position / 1000)} / ${elapsedLabel(total / 1000)}",
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
    }
}
