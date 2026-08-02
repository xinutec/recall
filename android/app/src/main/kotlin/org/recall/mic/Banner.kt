package org.recall.mic

import java.time.Duration
import java.time.Instant
import java.time.OffsetDateTime
import java.time.ZoneId
import java.time.format.DateTimeFormatter
import kotlin.math.max
import kotlin.math.roundToLong

/**
 * Elapsed recording time as "07:12" (or "1:07:12" past the hour) — seconds included,
 * because the readout doubles as the proof that the recorder is actually running.
 * Never negative: a clock that steps backwards mid-meeting shows 00:00, not a minus sign.
 */
fun elapsedLabel(since: Instant, now: Instant): String =
    elapsedLabel(Duration.between(since, now).seconds)

/** As above, for a duration already in seconds — a recording's length, a playback head. */
fun elapsedLabel(seconds: Long): String {
    val secs = max(0L, seconds)
    val h = secs / 3600
    val m = (secs % 3600) / 60
    val s = secs % 60
    return if (h > 0) {
        "%d:%02d:%02d".format(h, m, s)
    } else {
        "%02d:%02d".format(m, s)
    }
}

/** When a recording was made, for its row in the list: "2 Aug, 09:50". */
fun startedLabel(start: Instant, zone: ZoneId): String =
    DateTimeFormatter.ofPattern("d MMM, HH:mm").format(start.atZone(zone))

/**
 * A recording's size, rounded the way a person reads it: "21.4 MB", "812 KB". Shown
 * because it is the other half of "is this the recording I think it is" — a 40-minute
 * appointment that came out 300 KB did not record what you were expecting.
 */
fun sizeLabel(bytes: Long): String =
    when {
        bytes >= 1_000_000L -> "%.1f MB".format(bytes / 1_000_000.0)
        bytes >= 1_000L -> "${bytes / 1_000} KB"
        else -> "$bytes B"
    }

/**
 * The household paused-banner text, kept byte-for-byte identical to the website's
 * (frontend `app.html` + `format.ts`): "Recording paused — auto-resumes in 5h 23m
 * (by 2026-07-04 08:30)". One place so web/Android/iOS can't drift apart.
 */
object Banner {
    private val DATE_TIME = DateTimeFormatter.ofPattern("yyyy-MM-dd HH:mm")

    fun pausedText(pausedUntilIso: String?, now: Instant, zone: ZoneId): String {
        val until =
            pausedUntilIso?.let { runCatching { OffsetDateTime.parse(it) }.getOrNull() }
                ?: return "Recording paused"
        val by = until.atZoneSameInstant(zone).format(DATE_TIME)
        return "Recording paused — auto-resumes in ${remaining(until.toInstant(), now)} (by $by)"
    }

    /** Time left as "5h 23m" / "23m" / "now" — whole minutes, never negative
     *  (matches format.ts durationUntil). */
    private fun remaining(until: Instant, now: Instant): String {
        val mins =
            max(
                0L,
                (until.toEpochMilli() - now.toEpochMilli()).toDouble().div(60_000).roundToLong(),
            )
        if (mins == 0L) return "now"
        val h = mins / 60
        val m = mins % 60
        return if (h > 0) "${h}h ${m}m" else "${m}m"
    }
}
