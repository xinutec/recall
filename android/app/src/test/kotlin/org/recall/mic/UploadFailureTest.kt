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

/** Upload failures as text a person can act on ([UploadFailure]). */
class UploadFailureTest {
    @Test
    fun notAuthorisedNamesTheTokenBecauseThatIsTheFix() {
        val said = UploadFailure.describe(IllegalStateException("HTTP 401"))
        assertTrue(said, said.contains("token"))
        assertEquals(said, UploadFailure.describe(IllegalStateException("HTTP 403")))
    }

    @Test
    fun anUnreachableHostIsNotConfusedWithARefusal() {
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
        // Whatever the throwable carries, only a status code is taken from it.
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
        // A port is not a status.
        assertNull(UploadFailure.httpStatus("connect to isis.vpn:8000 timed out"))
    }
}
