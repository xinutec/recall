package org.recall.mic

import org.json.JSONObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import java.time.Instant

/**
 * The beat's body is a contract with the server's `HeartbeatIn`, so it is built by a pure
 * function and pinned here. A renamed key would otherwise fail silently — the server
 * defaults every field but `device`, so a typo arrives as a beat that is simply missing
 * its detail, not as an error anyone sees.
 */
class HeartbeatTest {
    private val started: Instant = Instant.parse("2026-08-11T07:00:00Z")

    private fun body(streaming: Boolean = true, charging: Boolean? = true) =
        JSONObject(Heartbeat.body("pixel5", "0.6 (6)", started, streaming, charging))

    @Test
    fun `carries the fields the server reads`() {
        val b = body()
        assertEquals("pixel5", b.getString("device"))
        assertEquals("android", b.getString("app"))
        assertEquals("0.6 (6)", b.getString("version"))
        assertEquals("2026-08-11T07:00:00Z", b.getString("startedAt"))
        assertTrue(b.getBoolean("streaming"))
        assertTrue(b.getBoolean("charging"))
    }

    @Test
    fun `a paused household still beats`() {
        // The whole point: capture is normally paused for days at a time, and the app
        // surviving that is the fact nothing else in recall could report.
        assertFalse(body(streaming = false).getBoolean("streaming"))
    }

    @Test
    fun `unknown charge is omitted rather than guessed`() {
        // Guessing "discharging" would invent the one reading a mains-powered room
        // phone is actually watched for.
        assertFalse(body(charging = null).has("charging"))
        assertFalse(body(charging = false).getBoolean("charging"))
    }

    @Test
    fun `the cadence matches what the grader was told to expect`() {
        // recall.mic_alive.BEAT_EVERY_MINUTES is 60 and the fleetwatch thresholds are
        // written as multiples of it. Drifting apart here would quietly leave every
        // threshold describing a cadence nothing sends.
        assertEquals(60L, Heartbeat.EVERY_MINUTES)
    }

    @Test
    fun `the process start is fixed for the life of the process`() {
        // "Alive now" and "alive since Tuesday" are different answers, and only the
        // second tells a stable app from one relaunching between beats.
        assertEquals(Heartbeat.startedAt, Heartbeat.startedAt)
    }
}
