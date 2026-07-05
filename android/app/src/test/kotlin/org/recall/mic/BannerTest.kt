package org.recall.mic

import java.time.Instant
import java.time.ZoneId
import org.junit.Assert.assertEquals
import org.junit.Test

class BannerTest {
    // The reference: the website's banner reads
    // "Recording paused — auto-resumes in 5h 23m (by 2026-07-04 08:30)".
    // The phones must match it byte-for-byte (durationUntil + dayKey HH:mm).
    private val london = ZoneId.of("Europe/London")

    @Test
    fun matchesWebsitePhrasingWithHoursAndMinutes() {
        // 2026-07-04 08:30 London, read 5h 23m earlier (03:07).
        val until = "2026-07-04T07:30:00+00:00" // 08:30 BST
        val now = Instant.parse("2026-07-04T02:07:00Z") // 03:07 BST
        assertEquals(
            "Recording paused — auto-resumes in 5h 23m (by 2026-07-04 08:30)",
            Banner.pausedText(until, now, london),
        )
    }

    @Test
    fun minutesOnlyWhenUnderAnHour() {
        val until = "2026-07-04T07:30:00+00:00"
        val now = Instant.parse("2026-07-04T07:07:00Z")
        assertEquals(
            "Recording paused — auto-resumes in 23m (by 2026-07-04 08:30)",
            Banner.pausedText(until, now, london),
        )
    }

    @Test
    fun readsNowWhenDeadlinePassed() {
        // Never a negative countdown — a past resume time reads "now", as on the web.
        val until = "2026-07-04T07:30:00+00:00"
        val now = Instant.parse("2026-07-04T09:00:00Z")
        assertEquals(
            "Recording paused — auto-resumes in now (by 2026-07-04 08:30)",
            Banner.pausedText(until, now, london),
        )
    }

    @Test
    fun bareWhenNoResumeTime() {
        // running==false with no pausedUntil (or an unparseable one) → no resume clause.
        val now = Instant.parse("2026-07-04T09:00:00Z")
        assertEquals("Recording paused", Banner.pausedText(null, now, london))
        assertEquals("Recording paused", Banner.pausedText("not-a-date", now, london))
    }
}
