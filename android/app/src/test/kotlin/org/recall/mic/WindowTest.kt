package org.recall.mic

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import java.time.Instant

/** The window, read and spelled as recall (audiocore's `Window`) does. */
class WindowTest {
    private val now = Instant.parse("2026-10-23T10:00:00Z")
    private val until = Instant.parse("2026-10-23T11:00:00Z")

    @Test
    fun `each window reads back as it was spelled`() {
        for (window in listOf(Window.Off, Window.Record(until), Window.Hold(until))) {
            assertEquals(window, Window.parse(window.spell()))
        }
        assertEquals("record 2026-10-23T11:00:00Z", Window.Record(until).spell())
    }

    @Test
    fun `unreadable text is no window at all`() {
        for (text in listOf("", "on", "record", "record soon", "2026-10-23T11:00:00Z")) {
            assertNull(text, Window.parse(text))
        }
    }

    @Test
    fun `only an open window records, and it closes at its end`() {
        assertTrue(Window.Record(until).records(now))
        assertFalse(Window.Record(until).records(until))
        assertFalse(Window.Hold(until).records(now))
        assertFalse(Window.Off.records(now))
        assertEquals(Window.Off, Window.Hold(until).at(until))
    }

    @Test
    fun `until a time is the next one, today or tomorrow`() {
        val late = java.time.ZonedDateTime.parse("2026-10-23T22:30:00+01:00[Europe/London]")
        assertEquals(Instant.parse("2026-10-23T22:15:00Z"), nextTimeOfDay(23, 15, late))
        assertEquals(Instant.parse("2026-10-24T00:00:00Z"), nextTimeOfDay(1, 0, late))
        assertEquals(Instant.parse("2026-10-24T21:30:00Z"), nextTimeOfDay(22, 30, late))
    }
}
