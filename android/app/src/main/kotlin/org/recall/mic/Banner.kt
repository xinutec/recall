package org.recall.mic

import java.time.Duration
import java.time.Instant
import java.time.OffsetDateTime
import java.time.ZoneId
import java.time.format.DateTimeFormatter
import kotlin.math.max
import kotlin.math.roundToLong

/**
 * Elapsed time as "07:12", or "1:07:12" past the hour; the ticking seconds show the
 * recorder runs. A clock stepped back shows 00:00.
 */
fun elapsedLabel(since: Instant, now: Instant): String =
    elapsedLabel(Duration.between(since, now).seconds)

/** As above, for a duration in seconds. */
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

/** A recording's size: "21.4 MB", "812 KB". Shown since a too-small file reveals a
 * failed recording. */
fun sizeLabel(bytes: Long): String =
    when {
        bytes >= 1_000_000L -> "%.1f MB".format(bytes / 1_000_000.0)
        bytes >= 1_000L -> "${bytes / 1_000} KB"
        else -> "$bytes B"
    }

/**
 * The paused-banner text, identical to the web app's (`app.html`, `format.ts`):
 * "Recording paused — auto-resumes in 5h 23m (by 2026-07-04 08:30)".
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

    /** Time left as "5h 23m", "23m" or "now", as format.ts `durationUntil`. */
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
