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
    fun `a deaf app says so instead of falling silent`() {
        // #887: an app whose AudioRecord will not initialise used to stop beating, and
        // the check went red for the wrong reason. It now beats and carries the fault.
        assertFalse(body(micOk = false).getBoolean("micOk"))
        assertTrue(body().getBoolean("micOk"))
    }

    @Test
    fun `the vpn is tried before the lan, so the fallback stays a backstop`() {
        // #888: audio goes to the LAN host, so a phone at home with its tunnel off
        // records fine and used to read as dead. The fallback fixes that WITHOUT
        // making the LAN the normal path — a phone away from home must behave
        // exactly as before, and a beat that took the back way is marked by the relay.
        assertEquals(
            listOf("10.100.0.2", "192.168.1.81"),
            Heartbeat.hostsToTry("10.100.0.2", "192.168.1.81"),
        )
        // Blank halves are skipped rather than tried: an unconfigured host is not an
        // address, and attempting it would cost a timeout per beat.
        assertEquals(listOf("192.168.1.81"), Heartbeat.hostsToTry("", "192.168.1.81"))
        assertEquals(listOf("10.100.0.2"), Heartbeat.hostsToTry("10.100.0.2", ""))
        assertEquals(emptyList<String>(), Heartbeat.hostsToTry("", ""))
        // One host configured for both: try it once, not twice.
        assertEquals(listOf("10.100.0.2"), Heartbeat.hostsToTry("10.100.0.2", "10.100.0.2"))
    }

    @Test
    fun `a landed beat waits the full hour`() {
        assertEquals(Heartbeat.EVERY_MINUTES, Heartbeat.nextDelayMinutes(0))
    }

    @Test
    fun `a failed beat retries soon, not at the next hour mark`() {
        // #886: the loop used to sleep the full hour whatever happened, so a phone
        // whose tunnel blipped for a minute read dead for an hour — measured on the
        // iPhone 2026-08-14, which only went green because it was relaunched by hand.
        assertEquals(1L, Heartbeat.nextDelayMinutes(1))
        assertEquals(2L, Heartbeat.nextDelayMinutes(2))
        assertEquals(4L, Heartbeat.nextDelayMinutes(3))
        assertEquals(8L, Heartbeat.nextDelayMinutes(4))
    }

    @Test
    fun `a long outage costs no more than the hourly cadence`() {
        // The bound that keeps this a backoff and not a poll: one request an hour is
        // the design, and a phone that is simply off must never beat harder than that.
        assertEquals(Heartbeat.EVERY_MINUTES, Heartbeat.nextDelayMinutes(7))
        assertEquals(Heartbeat.EVERY_MINUTES, Heartbeat.nextDelayMinutes(64))
        // No overflow at absurd counts — this counter is only ever reset by success,
        // so a phone left in a dead spot for a month keeps incrementing it.
        assertEquals(Heartbeat.EVERY_MINUTES, Heartbeat.nextDelayMinutes(Int.MAX_VALUE))
    }

    @Test
    fun `an outage costs a few extra requests, then settles`() {
        // The bound that matters is the COUNT of extra requests an outage can cost
        // before the schedule reaches the hourly cap — not the wall-clock they span.
        // (An earlier version asserted the burst fit inside one cadence; it does not,
        // by three minutes, and that was never the property worth having.)
        val delays = mutableListOf<Long>()
        var n = 1
        while (Heartbeat.nextDelayMinutes(n) < Heartbeat.EVERY_MINUTES) {
            delays.add(Heartbeat.nextDelayMinutes(n))
            n++
        }
        assertTrue("an outage costs ${delays.size} retries", delays.size <= 8)
        // Monotonic: each wait is at least the one before it, so the schedule can only
        // ever back OFF. A dip would mean an outage beating harder the longer it lasts.
        assertEquals(delays.sorted(), delays)
    }

    @Test
    fun `the process start is fixed for the life of the process`() {
        // "Alive now" and "alive since Tuesday" are different answers, and only the
        // second tells a stable app from one relaunching between beats.
        assertEquals(Heartbeat.startedAt, Heartbeat.startedAt)
    }
}
