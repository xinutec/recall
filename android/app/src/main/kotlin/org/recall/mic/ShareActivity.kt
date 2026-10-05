package org.recall.mic

import android.content.Intent
import android.net.Uri
import android.os.Build
import android.os.Bundle
import android.provider.DocumentsContract
import android.provider.MediaStore
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
 * "Recall" in the share sheet: uploads a shared audio file as a new session, with a
 * small progress screen that closes itself on success.
 *
 * The session is dated when the audio was recorded, not shared: from the recorder's
 * filename stamp, else the file's modification time ([modifiedMillis]).
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
        // Isis, not the recorder host; never blank.
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
                        withContext(Dispatchers.IO) {
                            ShareUpload.chooseStart(
                                name,
                                modifiedMillis(uri),
                                Instant.now(),
                                ZoneId.systemDefault(),
                            )
                        }
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

    /** Copy to the cache, so the upload does not depend on the content-URI grant
     * lasting. */
    private fun copyToCache(uri: Uri, name: String): File {
        val out = File(cacheDir, "share-${System.nanoTime()}-$name")
        contentResolver.openInputStream(uri)?.use { input ->
            out.outputStream().use { input.copyTo(it) }
        } ?: error("cannot open shared file")
        return out
    }

    /**
     * When the shared file was last written, or null. From the content URI, since the
     * cache copy is stamped now. A documents provider answers `last_modified`,
     * MediaStore `date_modified`, in different units.
     */
    private fun modifiedMillis(uri: Uri): Long? =
        ShareUpload.modifiedMillis(
            longColumn(uri, DocumentsContract.Document.COLUMN_LAST_MODIFIED),
            longColumn(uri, MediaStore.MediaColumns.DATE_MODIFIED),
        )

    /** One long column, or null, including when the provider lacks it (that throws). */
    private fun longColumn(uri: Uri, column: String): Long? =
        runCatching {
            contentResolver.query(uri, arrayOf(column), null, null, null)?.use { c ->
                if (c.moveToFirst() && c.columnCount > 0 && !c.isNull(0)) c.getLong(0) else null
            }
        }.getOrNull()

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
            @Suppress("DEPRECATION") // the typed overload needs TIRAMISU (branch above)
            intent.getParcelableExtra(Intent.EXTRA_STREAM)
        }

    @androidx.compose.runtime.Composable
    private fun ShareScreen() {
        Surface(modifier = Modifier.fillMaxSize()) {
            Column(
                // targetSdk 36 insets nothing, so a long message could run under the
                // status bar.
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
