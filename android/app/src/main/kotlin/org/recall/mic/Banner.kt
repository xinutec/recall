package org.recall.mic

import java.time.Instant
import java.time.OffsetDateTime
import java.time.ZoneId
import java.time.format.DateTimeFormatter
import kotlin.math.max
import kotlin.math.roundToLong

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
