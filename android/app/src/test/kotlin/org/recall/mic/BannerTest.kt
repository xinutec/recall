package org.recall.mic

import org.junit.Assert.assertEquals
import org.junit.Test
import java.time.Instant
import java.time.ZoneId

class BannerTest {
    // The texts must equal the web app's (`app.html`).
    private val london = ZoneId.of("Europe/London")

    private val end = Instant.parse("2026-07-04T07:30:00Z") // 08:30 BST

    @Test
    fun anOpenWindowSaysItsEndAndTheTimeLeft() {
        val now = Instant.parse("2026-07-04T02:07:00Z") // 03:07 BST
        assertEquals(
            "Recording until 08:30 · 5h 23m left",
            Banner.windowText(Window.Record(end), now, london),
        )
    }

    @Test
    fun anEndOnAnotherDayCarriesItsDate() {
        val now = Instant.parse("2026-07-03T22:00:00Z") // 23:00 BST, the day before
        assertEquals(
            "Paused · the window ends 2026-07-04 08:30",
            Banner.windowText(Window.Hold(end), now, london),
        )
    }

    @Test
    fun offAndAFaultSaySo() {
        val now = Instant.parse("2026-07-04T02:07:00Z")
        assertEquals("Not recording", Banner.windowText(Window.Off, now, london))
        // A window past its end is off.
        assertEquals("Not recording", Banner.windowText(Window.Record(end), end, london))
        assertEquals(
            "Recording with no window set — a recorder fault",
            Banner.windowText(null, now, london),
        )
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
        assertEquals("21.4 MB", sizeLabel(21_400_000))
        assertEquals("812 KB", sizeLabel(812_345))
        assertEquals("640 B", sizeLabel(640))
    }

    @Test
    fun elapsedNeverGoesNegative() {
        // A clock stepped back.
        val since = Instant.parse("2026-07-04T09:00:00Z")
        assertEquals("00:00", elapsedLabel(since, since.minusSeconds(90)))
    }
}
