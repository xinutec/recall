package org.recall.mic

import java.io.IOException
import java.net.ConnectException
import java.net.SocketTimeoutException
import java.net.UnknownHostException

/**
 * Why an upload failed, as one of the four causes with different fixes: a wrong token
 * (Settings), no route home (wait, or fix the control host), a refused file (look at
 * the recording), or a server fault (nothing to do). A bare 401 in the log once went
 * unnoticed for months.
 *
 * The text is written here, never copied from the throwable, which could carry the
 * bearer token onto the screen; only a status code is taken from it.
 */
object UploadFailure {
    private const val AUTH = "Not authorised — check the upload token in Settings."
    private const val UNREACHABLE =
        "Couldn't reach recall. Check the control host in Settings, or try again from home."
    private const val UNKNOWN = "Upload failed. It will keep trying."

    /** What [ShareUpload] raises for a non-2xx. */
    private val STATUS = Regex("""^HTTP (\d{3})$""")

    /** The status code [ShareUpload] put in the message, or null if this wasn't one. */
    fun httpStatus(message: String?): Int? =
        message?.let {
            STATUS
                .find(it.trim())
                ?.groupValues
                ?.get(1)
                ?.toIntOrNull()
        }

    /** What to show on the recording's row. Never empty, never the token. */
    fun describe(failure: Throwable): String {
        val status = httpStatus(failure.message)
        if (status != null) return forStatus(status)
        return when (failure) {
            // One situation from the phone, with one fix.
            is ConnectException, is UnknownHostException, is SocketTimeoutException -> UNREACHABLE

            is IOException -> UNKNOWN

            else -> UNKNOWN
        }
    }

    private fun forStatus(status: Int): String =
        when {
            status == 401 || status == 403 -> {
                AUTH
            }

            // The server refused the file; the phone still has it.
            status in 400..499 -> {
                "recall refused this recording (HTTP $status). Play it before deleting it."
            }

            status in 500..599 -> {
                "recall couldn't accept it (HTTP $status). Nothing to do here — it will keep trying."
            }

            else -> {
                UNKNOWN
            }
        }
}
