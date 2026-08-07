package org.recall.mic

import android.content.Intent
import android.net.Uri
import android.os.Build
import android.os.Bundle
import android.provider.OpenableColumns
import android.util.Log
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawingPadding
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import androidx.lifecycle.lifecycleScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import java.io.File
import java.time.Instant
import java.time.ZoneId

/**
 * Receives an audio file shared from another app (the mp3 recorder's Share sheet) and
 * uploads it to the recall host as a new session. Shown as "Recall" in the share sheet.
 *
 * The upload runs in the activity's lifecycle with a small progress screen: copy the
 * shared content to cache (so a transient content-URI grant can't fail a long stream),
 * upload, then auto-dismiss on success or offer Close on failure.
 */
class ShareActivity : ComponentActivity() {
    private sealed interface UiState {
        data class Uploading(
            val name: String,
        ) : UiState

        data class Done(
            val title: String,
        ) : UiState

        data class Failed(
            val reason: String,
        ) : UiState
    }

    private var state by mutableStateOf<UiState>(UiState.Uploading("…"))

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContent { RecallMicTheme { ShareScreen() } }

        val uri = streamUri(intent)
        // The API host (Isis), NOT the recorder host the PCM stream goes to: the Mac's
        // own UI was retired in the Isis split and its :8000 refuses, so uploads sent
        // there went nowhere. Never blank — it falls back to DEFAULT_CONTROL_HOST.
        val host = Prefs.controlHost(this)
        if (uri == null) {
            state = UiState.Failed("No audio file was shared.")
        } else {
            lifecycleScope.launch { run(uri, host) }
        }
    }

    private suspend fun run(uri: Uri, host: String) {
        val name = displayName(uri)
        state = UiState.Uploading(name)
        val result =
            runCatching {
                val cached = withContext(Dispatchers.IO) { copyToCache(uri, name) }
                try {
                    val start =
                        ShareUpload.chooseStart(name, null, Instant.now(), ZoneId.systemDefault())
                    ShareUpload
                        .upload(host, cached, name, start, Prefs.deviceToken(this@ShareActivity))
                        .getOrThrow()
                        .title
                } finally {
                    withContext(Dispatchers.IO) { cached.delete() }
                }
            }
        result
            .onSuccess { title ->
                state = UiState.Done(title)
                delay(1400)
                finish()
            }.onFailure {
                Log.w("recall.share", "upload failed", it)
                state = UiState.Failed("Couldn't reach the recall host on your network.")
            }
    }

    /** Copy the shared content to a cache file so the upload doesn't depend on holding
     * the (temporary) content-URI read grant for the whole stream. */
    private fun copyToCache(uri: Uri, name: String): File {
        val out = File(cacheDir, "share-${System.nanoTime()}-$name")
        contentResolver.openInputStream(uri)?.use { input ->
            out.outputStream().use { input.copyTo(it) }
        } ?: error("cannot open shared file")
        return out
    }

    private fun displayName(uri: Uri): String {
        contentResolver
            .query(uri, arrayOf(OpenableColumns.DISPLAY_NAME), null, null, null)
            ?.use { c ->
                if (c.moveToFirst() && c.columnCount > 0) {
                    c.getString(0)?.let { return it }
                }
            }
        return uri.lastPathSegment ?: "recording"
    }

    private fun streamUri(intent: Intent): Uri? =
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
            intent.getParcelableExtra(Intent.EXTRA_STREAM, Uri::class.java)
        } else {
            @Suppress("DEPRECATION")
            intent.getParcelableExtra(Intent.EXTRA_STREAM)
        }

    @androidx.compose.runtime.Composable
    private fun ShareScreen() {
        Surface(modifier = Modifier.fillMaxSize()) {
            Column(
                // The content is centred, so it does not currently reach the status
                // bar — but targetSdk 36 means nothing insets this window, so a
                // longer message would run under the icons.
                Modifier.fillMaxSize().safeDrawingPadding().padding(32.dp),
                verticalArrangement = Arrangement.spacedBy(16.dp, Alignment.CenterVertically),
                horizontalAlignment = Alignment.CenterHorizontally,
            ) {
                when (val s = state) {
                    is UiState.Uploading -> {
                        CircularProgressIndicator()
                        Text("Sending to recall…", style = MaterialTheme.typography.titleMedium)
                        Text(s.name, style = MaterialTheme.typography.bodySmall)
                    }

                    is UiState.Done -> {
                        Text(
                            "✓",
                            style = MaterialTheme.typography.displayMedium,
                            color = MaterialTheme.colorScheme.primary,
                        )
                        Text("Saved to recall", style = MaterialTheme.typography.titleMedium)
                        Text(s.title, style = MaterialTheme.typography.bodySmall)
                    }

                    is UiState.Failed -> {
                        Text(
                            "Couldn't save to recall",
                            style = MaterialTheme.typography.titleMedium,
                            color = MaterialTheme.colorScheme.error,
                        )
                        Text(s.reason, style = MaterialTheme.typography.bodyMedium)
                        Button(onClick = { finish() }) { Text("Close") }
                    }
                }
            }
        }
    }
}
