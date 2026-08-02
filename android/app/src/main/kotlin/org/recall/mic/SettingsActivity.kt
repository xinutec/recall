package org.recall.mic

import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle

/**
 * The two hosts and this device's identity, off the main screen — they are set once per
 * phone and then never again, so leaving them on the front page made the thing you look at
 * every day mostly configuration.
 *
 * Saved as they are typed (no Save button): every field here is a single value with an
 * immediate effect, and a form that can be left half-applied is a form that lies.
 */
class SettingsActivity : ComponentActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        enableEdgeToEdge()
        setContent {
            RecallMicTheme {
                SettingsScreen(
                    initialHost = Prefs.host(this),
                    initialControlHost = Prefs.controlHost(this),
                    deviceId = Prefs.deviceId(this),
                    onHostChanged = { Prefs.save(this, it, Prefs.enabled(this)) },
                    onControlHostChanged = { Prefs.saveControlHost(this, it) },
                    onBack = { finish() },
                )
            }
        }
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun SettingsScreen(
    initialHost: String,
    initialControlHost: String,
    deviceId: String,
    onHostChanged: (String) -> Unit,
    onControlHostChanged: (String) -> Unit,
    onBack: () -> Unit,
) {
    var host by remember { mutableStateOf(initialHost) }
    var controlHost by remember { mutableStateOf(initialControlHost) }
    val running by MicState.running.collectAsStateWithLifecycle()

    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text("Settings") },
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
            OutlinedTextField(
                value = host,
                onValueChange = {
                    host = it.trim()
                    onHostChanged(host)
                },
                label = { Text("recorder host (stream)") },
                placeholder = { Text("192.168.1.81") },
                singleLine = true,
                // Locked while streaming: the service connects to the host it was given
                // at Start, so an edit here would make the status card lie.
                enabled = !running,
                modifier = Modifier.fillMaxWidth(),
            )
            Text(
                if (running) {
                    "Stop the stream to change this."
                } else {
                    "Where the live microphone stream goes — the recorder on the home LAN."
                },
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )

            OutlinedTextField(
                value = controlHost,
                onValueChange = {
                    controlHost = it.trim()
                    onControlHostChanged(controlHost)
                },
                label = { Text("control host (Isis)") },
                placeholder = { Text(Prefs.DEFAULT_CONTROL_HOST) },
                singleLine = true,
                // Not tied to the stream, so it's editable any time.
                modifier = Modifier.fillMaxWidth(),
            )
            Text(
                "The recall web API: the pause controls, the device list, and where " +
                    "meeting recordings are uploaded.",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )

            Text(
                "This device: $deviceId",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
    }
}
