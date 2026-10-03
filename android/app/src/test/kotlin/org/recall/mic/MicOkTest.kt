package org.recall.mic

import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import java.io.IOException

/**
 * What a failed streaming attempt is allowed to conclude about the microphone.
 *
 * ⚠ A network failure says nothing about the mic. Reading it as "mic fine" once
 * hid a dead microphone behind the check built to catch one.
 */
class MicOkTest {
    @Test
    fun `a mic failure marks the mic bad`() {
        assertFalse(
            micOkAfter(previous = true, failure = MicUnavailableException("no AudioRecord")),
        )
    }

    @Test
    fun `a network failure does not clear an earlier mic failure`() {
        // ⚠ The bug. The socket connects before the mic opens, so during a
        // household pause — listener closed — every attempt fails on the connect.
        // Reading that as "the mic is fine" is what kept micOk=true through nine
        // hours of a phone that could not open AudioRecord.
        assertFalse(micOkAfter(previous = false, failure = IOException("connection refused")))
    }

    @Test
    fun `a network failure does not invent a mic failure either`() {
        // It says nothing in either direction: the attempt never reached the mic.
        assertTrue(micOkAfter(previous = true, failure = IOException("connection refused")))
    }

    @Test
    fun `a mic failure is sticky across later network failures`() {
        // The retry loop is what made this matter: one bad open followed by a
        // hundred refused connects must still report a bad mic.
        var ok = micOkAfter(previous = true, failure = MicUnavailableException("no AudioRecord"))
        repeat(100) { ok = micOkAfter(previous = ok, failure = IOException("connection refused")) }
        assertFalse(ok)
    }
}
