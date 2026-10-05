package org.recall.mic

import org.json.JSONObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import java.time.Instant

/**
 * The beat and its schedule. The body is pinned here because the server's `HeartbeatIn`
 * defaults every field but `device`, so a misspelt key would fail silently.
 */
class HeartbeatTest {
    private val started: Instant = Instant.parse("2026-08-11T07:00:00Z")

    private fun body(streaming: Boolean = true, charging: Boolean? = true, micOk: Boolean = true) =
        JSONObject(Heartbeat.body("pixel5", "0.6 (6)", started, streaming, charging, micOk))

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
        assertFalse(body(streaming = false).getBoolean("streaming"))
    }

    @Test
    fun `unknown charge is omitted rather than guessed`() {
        assertFalse(body(charging = null).has("charging"))
        assertFalse(body(charging = false).getBoolean("charging"))
    }

    @Test
    fun `the cadence matches what the grader was told to expect`() {
        // recalld::devices::BEAT_EVERY_MINUTES.
        assertEquals(60L, Heartbeat.EVERY_MINUTES)
    }

    @Test
    fun `a deaf app says so instead of falling silent`() {
        // #887
        assertFalse(body(micOk = false).getBoolean("micOk"))
        assertTrue(body().getBoolean("micOk"))
    }

    @Test
    fun `the vpn is tried before the lan, so the fallback stays a backstop`() {
        // #888
        assertEquals(
            listOf("10.100.0.2", "192.168.1.81"),
            Heartbeat.hostsToTry("10.100.0.2", "192.168.1.81"),
        )
        assertEquals(listOf("192.168.1.81"), Heartbeat.hostsToTry("", "192.168.1.81"))
        assertEquals(listOf("10.100.0.2"), Heartbeat.hostsToTry("10.100.0.2", ""))
        assertEquals(emptyList<String>(), Heartbeat.hostsToTry("", ""))
        assertEquals(listOf("10.100.0.2"), Heartbeat.hostsToTry("10.100.0.2", "10.100.0.2"))
    }

    @Test
    fun `a landed beat waits the full hour`() {
        assertEquals(Heartbeat.EVERY_MINUTES, Heartbeat.nextDelayMinutes(0))
    }

    @Test
    fun `a failed beat retries soon, not at the next hour mark`() {
        // #886
        assertEquals(1L, Heartbeat.nextDelayMinutes(1))
        assertEquals(2L, Heartbeat.nextDelayMinutes(2))
        assertEquals(4L, Heartbeat.nextDelayMinutes(3))
        assertEquals(8L, Heartbeat.nextDelayMinutes(4))
    }

    @Test
    fun `a long outage costs no more than the hourly cadence`() {
        assertEquals(Heartbeat.EVERY_MINUTES, Heartbeat.nextDelayMinutes(7))
        assertEquals(Heartbeat.EVERY_MINUTES, Heartbeat.nextDelayMinutes(64))
        // The counter grows without bound in a dead spot.
        assertEquals(Heartbeat.EVERY_MINUTES, Heartbeat.nextDelayMinutes(Int.MAX_VALUE))
    }

    @Test
    fun `an outage costs a few extra requests, then settles`() {
        // Bounded in requests before the hourly cap, not in time.
        val delays = mutableListOf<Long>()
        var n = 1
        while (Heartbeat.nextDelayMinutes(n) < Heartbeat.EVERY_MINUTES) {
            delays.add(Heartbeat.nextDelayMinutes(n))
            n++
        }
        assertTrue("an outage costs ${delays.size} retries", delays.size <= 8)
        // Never shorter than the wait before.
        assertEquals(delays.sorted(), delays)
    }

    @Test
    fun `the process start is fixed for the life of the process`() {
        assertEquals(Heartbeat.startedAt, Heartbeat.startedAt)
    }
}
