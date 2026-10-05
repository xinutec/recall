package org.recall.mic

import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import java.io.IOException

/** What a failed streaming attempt says about the microphone ([micOkAfter]). */
class MicOkTest {
    @Test
    fun `a mic failure marks the mic bad`() {
        assertFalse(
            micOkAfter(previous = true, failure = MicUnavailableException("no AudioRecord")),
        )
    }

    @Test
    fun `a network failure does not clear an earlier mic failure`() {
        // During a pause every attempt fails on the connect; this once hid a broken
        // mic for nine hours.
        assertFalse(micOkAfter(previous = false, failure = IOException("connection refused")))
    }

    @Test
    fun `a network failure does not invent a mic failure either`() {
        assertTrue(micOkAfter(previous = true, failure = IOException("connection refused")))
    }

    @Test
    fun `a mic failure is sticky across later network failures`() {
        var ok = micOkAfter(previous = true, failure = MicUnavailableException("no AudioRecord"))
        repeat(100) { ok = micOkAfter(previous = ok, failure = IOException("connection refused")) }
        assertFalse(ok)
    }
}
