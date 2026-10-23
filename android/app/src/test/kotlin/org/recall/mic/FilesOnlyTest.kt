package org.recall.mic

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import java.time.Instant

/** A phone with no recorder host records files on its own ([FilesOnly]). */
class FilesOnlyTest {
    private val now = Instant.parse("2026-10-23T10:00:00Z")
    private val open = "record 2026-10-23T11:00:00Z"

    private fun answer(window: String?) =
        CaptureState(
            running = false,
            until = null,
            held = false,
            desiredRunning = window == null,
            desiredUntil = window?.let { Window.parse(it)?.end?.toString() },
            desiredHeld = window?.startsWith("hold") == true,
            settled = true,
            micReachable = true,
            stateToken = "",
        )

    @Test
    fun `no recorder host means files only`() {
        assertTrue(FilesOnly.applies(""))
        assertTrue(FilesOnly.applies("  "))
        assertFalse(FilesOnly.applies("192.168.1.10"))
    }

    @Test
    fun `recall's answer is kept`() {
        assertEquals(open, FilesOnly.kept(saved = "off", answer = answer(open)))
        assertEquals("off", FilesOnly.kept(saved = open, answer = answer("off")))
    }

    @Test
    fun `out of reach, the last known window stands and closes at its end`() {
        assertEquals(open, FilesOnly.kept(saved = open, answer = null))
        assertTrue(FilesOnly.records(open, now))
        assertFalse(FilesOnly.records(open, Instant.parse("2026-10-23T11:00:00Z")))
        assertFalse(FilesOnly.records("off", now))
    }

    /** A dead battery or a forgotten phone records nothing: off was the last word. */
    @Test
    fun `a window that ended while the phone was off stays off`() {
        assertFalse(FilesOnly.records(open, Instant.parse("2026-10-30T09:00:00Z")))
    }

    /** A fault records: a phone that never heard from recall, or a broken answer. */
    @Test
    fun `nothing known, or a fault, records`() {
        assertTrue(FilesOnly.records(null, now))
        val fault = FilesOnly.kept(saved = "off", answer = answer(null))
        assertTrue(FilesOnly.records(fault, now))
    }
}
