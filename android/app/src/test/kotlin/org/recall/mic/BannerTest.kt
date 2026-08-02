package org.recall.mic

import org.junit.Assert.assertEquals
import org.junit.Test
import java.time.Instant
import java.time.ZoneId

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

    @Test
    fun elapsedCountsSecondsThenHours() {
        val since = Instant.parse("2026-07-04T09:00:00Z")
        assertEquals("00:00", elapsedLabel(since, since))
        assertEquals("07:12", elapsedLabel(since, since.plusSeconds(432)))
        assertEquals("59:59", elapsedLabel(since, since.plusSeconds(3599)))
        assertEquals("1:00:00", elapsedLabel(since, since.plusSeconds(3600)))
        assertEquals("2:07:12", elapsedLabel(since, since.plusSeconds(7632)))
    }

    @Test
    fun labelsARecordingsDateAndSize() {
        assertEquals("3 Jul, 09:50", startedLabel(Instant.parse("2026-07-03T08:50:50Z"), london))
        // Rounded the way a person reads it — the point is "is this the recording I think
        // it is", not an exact byte count.
        assertEquals("21.4 MB", sizeLabel(21_400_000))
        assertEquals("812 KB", sizeLabel(812_345))
        assertEquals("640 B", sizeLabel(640))
    }

    @Test
    fun elapsedNeverGoesNegative() {
        // A clock that steps backwards mid-meeting must not paint a minus sign onto the
        // one readout that says whether the recorder is running.
        val since = Instant.parse("2026-07-04T09:00:00Z")
        assertEquals("00:00", elapsedLabel(since, since.minusSeconds(90)))
    }
}
