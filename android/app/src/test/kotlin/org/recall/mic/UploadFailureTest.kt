package org.recall.mic

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import java.io.IOException
import java.net.ConnectException
import java.net.SocketTimeoutException
import java.net.UnknownHostException

/**
 * Turning an upload failure into something the person holding the phone can act on.
 *
 * On 2026-07-07 the meeting recorder had been 401ing for as long as the feature had
 * existed. `MeetingUpload` wrote `HTTP 401` to logcat and the screen showed a pending
 * count, so finding out cost a root-cause session — reading webauth.py's plane split,
 * checking _DEVICE_EXEMPT, comparing against the cookie the web Upload button carries —
 * to reach a conclusion the phone already knew and had written down where nobody looks.
 */
class UploadFailureTest {
    @Test
    fun notAuthorisedNamesTheTokenBecauseThatIsTheFix() {
        // The 2026-07-07 failure exactly. "Check the upload token" is the whole
        // difference between a one-minute fix and an afternoon.
        val said = UploadFailure.describe(IllegalStateException("HTTP 401"))
        assertTrue(said, said.contains("token"))
        assertEquals(said, UploadFailure.describe(IllegalStateException("HTTP 403")))
    }

    @Test
    fun anUnreachableHostIsNotConfusedWithARefusal() {
        // Different fix: one is the token, the other is "you aren't home yet" or the
        // control host is wrong. Telling them apart is most of the value here.
        val notHome = UploadFailure.describe(ConnectException("Failed to connect to /10.0.0.1"))
        assertTrue(notHome, notHome.contains("reach"))
        assertFalse(notHome, notHome.contains("token"))
        assertEquals(notHome, UploadFailure.describe(UnknownHostException("isis.vpn")))
        assertEquals(notHome, UploadFailure.describe(SocketTimeoutException("timeout")))
    }

    @Test
    fun aRejectedRecordingPointsAtTheFileAndCarriesTheCode() {
        val said = UploadFailure.describe(IllegalStateException("HTTP 400"))
        assertTrue(said, said.contains("400"))
        // The phone's copy is the one to look at, and it must not read as "delete me".
        assertFalse(said, said.contains("token"))
    }

    @Test
    fun aServerErrorSaysThereIsNothingToDoHere() {
        val said = UploadFailure.describe(IllegalStateException("HTTP 503"))
        assertTrue(said, said.contains("503"))
        assertTrue(said, said.contains("keep"))
    }

    @Test
    fun anUnrecognisedFailureStillSaysSomething() {
        val said = UploadFailure.describe(IOException("unexpected end of stream"))
        assertTrue(said, said.isNotEmpty())
    }

    @Test
    fun theTokenCanNeverReachTheScreen() {
        // ⚠ The property, not a spot check: the text is composed from this file's own
        // constants plus a status code, and never from the throwable's message. A
        // failure that quoted what it was given would put a bearer token on a screen —
        // and eventually in a screenshot, which is how a secret leaves a phone.
        val secret = "hunter2-RECALL-DEVICE-TOKEN"
        for (
        thrown in listOf(
            IllegalStateException("HTTP 401 Bearer $secret"),
            ConnectException("connect failed with $secret"),
            IOException(secret),
        )
        ) {
            val said = UploadFailure.describe(thrown)
            assertFalse(said, said.contains(secret))
            assertFalse(said, said.contains("hunter2"))
        }
    }

    @Test
    fun theStatusCodeIsReadOffTheMessageOrNothingIs() {
        assertEquals(401, UploadFailure.httpStatus("HTTP 401"))
        assertEquals(503, UploadFailure.httpStatus("HTTP 503"))
        assertNull(UploadFailure.httpStatus("Failed to connect to /10.0.0.1:8000"))
        assertNull(UploadFailure.httpStatus(null))
        // Not a status: a host:port that happens to contain three digits.
        assertNull(UploadFailure.httpStatus("connect to isis.vpn:8000 timed out"))
    }
}
