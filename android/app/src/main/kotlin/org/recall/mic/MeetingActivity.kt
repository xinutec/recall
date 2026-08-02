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
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import androidx.core.content.ContextCompat
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import kotlinx.coroutines.delay
import java.time.Instant

/**
 * The meeting-recorder screen: a title, one big button, the elapsed time, the shared level
 * meter, and — the part that must never be silent — how many finished recordings are still
 * waiting to reach the host.
 *
 * The recording itself belongs to [MeetingService], which outlives this screen; everything
 * here reflects [MeetingState] and sends intents. That is deliberate: an appointment is
 * recorded with the phone in a pocket and the screen off.
 */
class MeetingActivity : ComponentActivity() {
    private var pendingTitle = ""

    private val requestPermissions =
        registerForActivityResult(ActivityResultContracts.RequestMultiplePermissions()) { grants ->
            if (grants[Manifest.permission.RECORD_AUDIO] == true) {
                MeetingService.start(this, pendingTitle)
            } else {
                MeetingState.setError("Recording needs microphone permission.")
            }
        }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        enableEdgeToEdge()
        setContent {
            RecallMicTheme {
                MeetingScreen(onStart = ::begin, onStop = ::end, onRetry = ::drainQueue)
            }
        }
    }

    override fun onStart() {
        super.onStart()
        // Opening the screen is the clearest "am I home yet?" signal there is, so it both
        // shows the true queue depth and takes another run at it.
        drainQueue()
    }

    private fun begin(title: String) {
        Log.i(UI_LOG, "button: Record meeting")
        pendingTitle = title
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
            MeetingService.start(this, title)
        }
    }

    private fun end(title: String) {
        Log.i(UI_LOG, "button: Stop meeting")
        MeetingService.stop(this, title)
    }

    private fun drainQueue() {
        MeetingUpload.enqueue(this)
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun MeetingScreen(onStart: (String) -> Unit, onStop: (String) -> Unit, onRetry: () -> Unit) {
    val recording by MeetingState.recording.collectAsStateWithLifecycle()
    val startedAt by MeetingState.startedAt.collectAsStateWithLifecycle()
    val level by MeetingState.level.collectAsStateWithLifecycle()
    val pending by MeetingState.pending.collectAsStateWithLifecycle()
    val error by MeetingState.error.collectAsStateWithLifecycle()
    var title by remember { mutableStateOf("") }

    // A ticking clock for the elapsed readout — a second, since it counts seconds.
    var now by remember { mutableStateOf(Instant.now()) }
    LaunchedEffect(recording) {
        while (recording) {
            now = Instant.now()
            delay(1_000)
        }
    }

    Scaffold(topBar = { TopAppBar(title = { Text("Record a meeting") }) }) { inner ->
        Column(
            Modifier
                .padding(inner)
                .fillMaxSize()
                .verticalScroll(rememberScrollState())
                .padding(20.dp),
            verticalArrangement = Arrangement.spacedBy(16.dp),
        ) {
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

            OutlinedTextField(
                value = title,
                onValueChange = { title = it },
                label = { Text("what is this? (optional)") },
                placeholder = { Text("Oncology clinic — Dr …") },
                singleLine = true,
                modifier = Modifier.fillMaxWidth(),
            )
            Text(
                "Left blank, recall names it by date and time. It can be renamed there later.",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )

            if (recording) {
                Button(
                    onClick = { onStop(title) },
                    modifier = Modifier.fillMaxWidth(),
                ) { Text("Stop and save") }
            } else {
                Button(
                    onClick = { onStart(title) },
                    modifier = Modifier.fillMaxWidth(),
                ) { Text("Start recording") }
            }

            error?.let {
                Text(
                    it,
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.error,
                )
            }

            // Never silent: an upload that can't get home is visible here until it does.
            if (pending > 0) {
                Card(
                    colors =
                        CardDefaults.cardColors(
                            containerColor = MaterialTheme.colorScheme.surfaceVariant,
                        ),
                    modifier = Modifier.fillMaxWidth(),
                ) {
                    Column(
                        Modifier.padding(16.dp),
                        verticalArrangement = Arrangement.spacedBy(10.dp),
                    ) {
                        Text(
                            if (pending == 1) {
                                "1 recording waiting to upload"
                            } else {
                                "$pending recordings waiting to upload"
                            },
                            style = MaterialTheme.typography.titleSmall,
                        )
                        Text(
                            "They upload by themselves once recall is reachable. " +
                                "Nothing is deleted from the phone until the host has it.",
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                        Row(horizontalArrangement = Arrangement.spacedBy(12.dp)) {
                            OutlinedButton(onClick = onRetry) { Text("Try now") }
                        }
                    }
                }
            }
        }
    }
}
