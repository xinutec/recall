package org.recall.mic

import java.time.Duration
import java.time.Instant
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
 * The recording banner's text, as the web app's (`app.html`): "Recording until 11:00 ·
 * 45m left", "Paused · the window ends 11:00", with the date when the end is not today.
 */
object Banner {
    private val TIME = DateTimeFormatter.ofPattern("HH:mm")
    private val DATE_TIME = DateTimeFormatter.ofPattern("yyyy-MM-dd HH:mm")

    /** For the window asked for; null is a fault, which records. */
    fun windowText(window: Window?, now: Instant, zone: ZoneId): String =
        when (val at = window?.at(now)) {
            null -> {
                "Recording with no window set — a recorder fault"
            }

            Window.Off -> {
                "Not recording"
            }

            is Window.Record -> {
                "Recording until ${endsAt(at.end, now, zone)} · ${remaining(at.end, now)} left"
            }

            is Window.Hold -> {
                "Paused · the window ends ${endsAt(at.end, now, zone)}"
            }
        }

    /** The end's time, with the date unless it is today. */
    private fun endsAt(until: Instant, now: Instant, zone: ZoneId): String {
        val end = until.atZone(zone)
        val today = end.toLocalDate() == now.atZone(zone).toLocalDate()
        return end.format(if (today) TIME else DATE_TIME)
    }

    /** Time left as "5h 23m", "2h 0m", "23m" or "now", as format.ts `durationUntil`. */
    fun remaining(until: Instant, now: Instant): String {
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
