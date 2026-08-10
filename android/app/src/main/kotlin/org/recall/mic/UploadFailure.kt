package org.recall.mic

import java.io.IOException
import java.net.ConnectException
import java.net.SocketTimeoutException
import java.net.UnknownHostException

/**
 * Why an upload didn't land, in words the person holding the phone can act on.
 *
 * The meeting recorder 401ed for as long as the feature had existed and nobody knew.
 * `MeetingUpload` wrote `meeting upload failed: <file>: HTTP 401` to logcat and the
 * screen showed a pending count, which reads exactly like "not home yet" — so the
 * failure was found on 2026-07-07 by a root-cause session that ended at a conclusion
 * the phone had known all along and written down where nobody reads.
 *
 * The distinctions that matter to someone holding the phone are the ones with different
 * fixes: **the token is wrong** (Settings), **there is no route home** (wait, or the
 * control host is wrong), **recall refused this file** (look at the recording), and
 * **recall is broken** (nothing to do here). A single "upload failed" collapses four
 * different next actions into none.
 *
 * ⚠ **The text is composed here and never quoted from the throwable.** A message that
 * echoed what it was given would eventually put the bearer token on the screen — and
 * from there into a screenshot, which is how a secret leaves a phone. The only thing
 * taken from the failure is a three-digit status code.
 */
object UploadFailure {
    private const val AUTH = "Not authorised — check the upload token in Settings."
    private const val UNREACHABLE =
        "Couldn't reach recall. Check the control host in Settings, or try again from home."
    private const val UNKNOWN = "Upload failed. It will keep trying."

    /** `HTTP <code>` is what [ShareUpload] raises for a non-2xx; anything else has none. */
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
            // Every "the host isn't there" shape lands on one sentence: from the phone
            // they are one situation, and the fix is the same for all of them.
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

            // The server probed the body and refused it, so the phone's copy is the
            // thing to look at — and it is still here, which is what to say.
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
