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
fun elapsedLabel(since: Instant, now: Instant): String {
    val secs = max(0L, Duration.between(since, now).seconds)
    val h = secs / 3600
    val m = (secs % 3600) / 60
    val s = secs % 60
    return if (h > 0) {
        "%d:%02d:%02d".format(h, m, s)
    } else {
        "%02d:%02d".format(m, s)
    }
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
