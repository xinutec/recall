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
import androidx.compose.animation.core.animateFloatAsState
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
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
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.unit.dp
import androidx.core.content.ContextCompat
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import java.time.Duration
import java.time.Instant
import java.time.OffsetDateTime
import java.time.ZoneId
import kotlin.math.roundToInt

/**
 * Compose UI: a live status card (connection state + a mic level meter) over the
 * host/port config and Start/Stop. The heavy lifting is in StreamService; the UI
 * just reflects MicState and toggles the service.
 */
class MainActivity : ComponentActivity() {
    private val requestPermissions =
        registerForActivityResult(ActivityResultContracts.RequestMultiplePermissions()) { grants ->
            if (grants[Manifest.permission.RECORD_AUDIO] == true) StreamService.start(this)
        }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        enableEdgeToEdge()
        setContent {
            RecallMicTheme {
                MicScreen(
                    initialHost = Prefs.host(this),
                    deviceId = Prefs.deviceId(this),
                    onStart = ::beginStream,
                    onStop = ::endStream,
                )
            }
        }
        resumeIfEnabled()
    }

    private fun beginStream(host: String) {
        Log.i(UI_LOG, "button: Start ($host)")
        Prefs.save(this, host, enabled = true)
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
            StreamService.start(this)
        }
    }

    private fun endStream(host: String) {
        Log.i(UI_LOG, "button: Stop")
        Prefs.save(this, host, enabled = false)
        StreamService.stop(this)
    }

    /** Resume streaming on open if it was left enabled and the mic permission is in place. */
    private fun resumeIfEnabled() {
        val ready =
            Prefs.enabled(this) &&
                Prefs.host(this).isNotEmpty() &&
                ContextCompat.checkSelfPermission(this, Manifest.permission.RECORD_AUDIO) ==
                PackageManager.PERMISSION_GRANTED
        if (ready) StreamService.start(this)
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun MicScreen(
    initialHost: String,
    deviceId: String,
    onStart: (String) -> Unit,
    onStop: (String) -> Unit,
) {
    var host by remember { mutableStateOf(initialHost) }
    val running by MicState.running.collectAsStateWithLifecycle()
    val connected by MicState.connected.collectAsStateWithLifecycle()
    val level by MicState.level.collectAsStateWithLifecycle()

    // The household capture (pause) state lives in MicState — the one value the
    // notification renders too, so the two can't disagree. Poll it while the screen's
    // open (the service polls while it runs); only overwrite on a successful read so a
    // transient blip doesn't flicker the banner.
    val capture by MicState.capture.collectAsStateWithLifecycle()
    val scope = rememberCoroutineScope()
    LaunchedEffect(host) {
        if (host.isBlank()) return@LaunchedEffect
        delay(500) // debounce: typing restarts this effect per keystroke
        while (true) {
            CaptureApi.state(host)?.let { MicState.setCapture(it) }
            delay(5_000)
        }
    }

    // A ticking clock so the "auto-resumes in Xh Ym" countdown counts down between
    // capture polls (the poll dedups equal state, so it alone wouldn't advance it).
    var now by remember { mutableStateOf(Instant.now()) }
    LaunchedEffect(Unit) {
        while (true) {
            now = Instant.now()
            delay(30_000) // minute-granularity text; 30s keeps it within a minute of true
        }
    }

    // Fleet liveness: which recorders are streaming. Polled fast (~1.5s) so the
    // panel tracks devices going active within a second or two.
    var devices by remember { mutableStateOf<List<SourceStatus>>(emptyList()) }
    LaunchedEffect(host) {
        if (host.isBlank()) return@LaunchedEffect
        delay(500) // debounce, as above
        while (true) {
            // null = request failed → keep the last list so a blip can't blank the panel.
            CaptureApi.sources(host)?.let { devices = it }
            delay(1_500)
        }
    }

    // We know our own source id directly (it's what we announce in the handshake), so
    // we can highlight our own row in the Devices list with no extra call.
    val selfId = deviceId

    // Run a capture action (pause/resume), then publish its result to the one shared
    // state — so the screen and notification both reflect it.
    fun control(call: suspend (String) -> CaptureState?) {
        scope.launch { call(host)?.let { MicState.setCapture(it) } }
    }

    Scaffold(topBar = { TopAppBar(title = { Text("Recall Mic") }) }) { inner ->
        Column(
            Modifier
                .padding(inner)
                .fillMaxSize()
                .verticalScroll(rememberScrollState())
                .padding(20.dp),
            verticalArrangement = Arrangement.spacedBy(16.dp),
        ) {
            StatusCard(
                running,
                connected,
                paused = capture?.let { !it.running } == true,
                level,
                host,
            )
            CaptureBanner(
                capture = capture,
                now = now,
                onPause = {
                    Log.i(UI_LOG, "button: Pause recording")
                    control(CaptureApi::pause)
                },
                onSnooze = {
                    Log.i(UI_LOG, "button: Still away (snooze 24h)")
                    control(CaptureApi::pause)
                },
                onResume = {
                    Log.i(UI_LOG, "button: Resume now")
                    control(CaptureApi::resume)
                },
            )
            DevicesPanel(devices, selfId)
            OutlinedTextField(
                value = host,
                onValueChange = { host = it.trim() },
                label = { Text("recall host") },
                placeholder = { Text("192.168.1.81") },
                singleLine = true,
                // Locked while streaming (as on iOS): the service connects to the
                // SAVED host, so an edit here would make the status card lie.
                enabled = !running,
                modifier = Modifier.fillMaxWidth(),
            )
            Text(
                text = "This device: $selfId",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            Row(horizontalArrangement = Arrangement.spacedBy(12.dp)) {
                Button(
                    onClick = { onStart(host.trim()) },
                    enabled = host.isNotBlank() && !running,
                    modifier = Modifier.weight(1f),
                ) { Text("Start") }
                OutlinedButton(
                    onClick = { onStop(host.trim()) },
                    enabled = running,
                    modifier = Modifier.weight(1f),
                ) { Text("Stop") }
            }
        }
    }
}

@Composable
private fun StatusCard(
    running: Boolean,
    connected: Boolean,
    paused: Boolean,
    level: Float,
    host: String,
) {
    val (label, detail, accent) =
        when {
            connected -> {
                Triple("Streaming", "to $host", MaterialTheme.colorScheme.primary)
            }

            // A deliberate pause closes the host's listener, so don't read it as an error.
            running && paused -> {
                Triple(
                    "Paused",
                    "household recording is off",
                    MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }

            running -> {
                Triple(
                    "Waiting for recall host",
                    "trying $host…",
                    MaterialTheme.colorScheme.tertiary,
                )
            }

            else -> {
                Triple("Stopped", "not recording", MaterialTheme.colorScheme.onSurfaceVariant)
            }
        }

    Card(Modifier.fillMaxWidth()) {
        Column(
            Modifier.padding(20.dp),
            verticalArrangement = Arrangement.spacedBy(20.dp),
        ) {
            Row(
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(16.dp),
            ) {
                Box(
                    Modifier
                        .size(56.dp)
                        .clip(CircleShape)
                        .background(accent.copy(alpha = 0.15f)),
                    contentAlignment = Alignment.Center,
                ) {
                    Icon(
                        painterResource(R.drawable.ic_mic),
                        contentDescription = null,
                        tint = accent,
                        modifier = Modifier.size(30.dp),
                    )
                }
                Column {
                    Text(label, style = MaterialTheme.typography.titleLarge)
                    Text(
                        detail,
                        style = MaterialTheme.typography.bodyMedium,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
            }
            Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
                Text(
                    "mic level",
                    style = MaterialTheme.typography.labelMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                LevelMeter(if (connected) level else 0f)
            }
        }
    }
}

@Composable
private fun LevelMeter(level: Float, segments: Int = 24) {
    val animated by animateFloatAsState(targetValue = level, label = "mic-level")
    val lit = (animated * segments).roundToInt()
    Row(
        Modifier.fillMaxWidth(),
        horizontalArrangement = Arrangement.spacedBy(3.dp),
    ) {
        for (i in 0 until segments) {
            Box(
                Modifier
                    .weight(1f)
                    .height(32.dp)
                    .clip(RoundedCornerShape(3.dp))
                    .background(segmentColor(meterTier(i, lit, segments))),
            )
        }
    }
}

@Composable
private fun segmentColor(tier: MeterTier) =
    when (tier) {
        MeterTier.OFF -> MaterialTheme.colorScheme.onSurfaceVariant.copy(alpha = 0.22f)
        MeterTier.LOW -> MaterialTheme.colorScheme.primary
        MeterTier.MID -> MaterialTheme.colorScheme.tertiary
        MeterTier.HIGH -> MaterialTheme.colorScheme.error
    }

/**
 * Mirrors the web app's pause banner for the *household* capture (the whole system).
 * Shown only when the API is reachable (home), so a pause reads as a deliberate pause
 * — not the "Waiting for recall host" the stream shows when the port is closed.
 */
@Composable
private fun CaptureBanner(
    capture: CaptureState?,
    now: Instant,
    onPause: () -> Unit,
    onSnooze: () -> Unit,
    onResume: () -> Unit,
) {
    if (capture == null) return // API unreachable — nothing beyond the stream status
    val paused = !capture.running
    val container =
        if (paused) {
            MaterialTheme.colorScheme.errorContainer
        } else {
            MaterialTheme.colorScheme.surfaceVariant
        }
    Card(
        colors = CardDefaults.cardColors(containerColor = container),
        modifier = Modifier.fillMaxWidth(),
    ) {
        Column(Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(10.dp)) {
            Text(
                if (paused) {
                    Banner.pausedText(capture.pausedUntil, now, ZoneId.systemDefault())
                } else {
                    "Recording active"
                },
                style = MaterialTheme.typography.titleMedium,
            )
            if (paused) {
                Row(horizontalArrangement = Arrangement.spacedBy(12.dp)) {
                    OutlinedButton(onClick = onSnooze, modifier = Modifier.weight(1f)) {
                        Text("Still away (24h)")
                    }
                    Button(onClick = onResume, modifier = Modifier.weight(1f)) {
                        Text("Resume now")
                    }
                }
            } else {
                OutlinedButton(onClick = onPause, modifier = Modifier.fillMaxWidth()) {
                    Text("Pause recording")
                }
            }
        }
    }
}

/** The fleet: which recorders are streaming now and when each was last active. */
@Composable
private fun DevicesPanel(sources: List<SourceStatus>, selfId: String?) {
    if (sources.isEmpty()) {
        return
    }
    Card(
        colors = CardDefaults.cardColors(containerColor = MaterialTheme.colorScheme.surfaceVariant),
        modifier = Modifier.fillMaxWidth(),
    ) {
        Column(Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(10.dp)) {
            Text("Devices", style = MaterialTheme.typography.titleSmall)
            for (source in sources) {
                val isSelf = source.id == selfId
                Row(
                    verticalAlignment = Alignment.CenterVertically,
                    horizontalArrangement = Arrangement.spacedBy(10.dp),
                ) {
                    Box(
                        Modifier
                            .size(10.dp)
                            .clip(CircleShape)
                            .background(
                                if (source.active) {
                                    MaterialTheme.colorScheme.primary
                                } else {
                                    MaterialTheme.colorScheme.onSurfaceVariant.copy(alpha = 0.3f)
                                },
                            ),
                    )
                    Text(
                        source.name,
                        style = MaterialTheme.typography.bodyMedium,
                        fontWeight = if (isSelf) FontWeight.Bold else FontWeight.Normal,
                    )
                    if (isSelf) {
                        Text(
                            "this device",
                            style = MaterialTheme.typography.labelSmall,
                            color = MaterialTheme.colorScheme.primary,
                        )
                    }
                    Spacer(Modifier.weight(1f))
                    Text(
                        activityLabel(source),
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
            }
        }
    }
}

/** "active", or how long since a source last streamed — for the devices panel. */
private fun activityLabel(source: SourceStatus): String {
    if (source.active) {
        return "active"
    }
    val iso = source.lastActive ?: return "no signal"
    return runCatching {
        val secs =
            Duration.between(OffsetDateTime.parse(iso).toInstant(), Instant.now()).seconds
        when {
            secs < 60 -> "${secs}s ago"
            secs < 3600 -> "${secs / 60}m ago"
            else -> "${secs / 3600}h ago"
        }
    }.getOrDefault("idle")
}
