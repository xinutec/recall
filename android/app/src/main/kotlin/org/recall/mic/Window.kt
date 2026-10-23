package org.recall.mic

import java.time.Instant
import java.time.OffsetDateTime
import java.time.ZonedDateTime
import java.time.temporal.ChronoUnit

/**
 * What a person asked capture to do, as recall's `Window` (audiocore): off, record
 * until a time, or hold a window open without recording. Off unless someone opened
 * one. Spelled `off`, `record <until>` or `hold <until>`; text that is none of these
 * is no window at all, a fault, and a fault records.
 */
sealed interface Window {
    /** When the window closes; none while off. */
    val end: Instant?

    data object Off : Window {
        override val end: Instant? = null
    }

    data class Record(
        override val end: Instant,
    ) : Window

    data class Hold(
        override val end: Instant,
    ) : Window

    /** The window as it stands at [now]: past its end, off. */
    fun at(now: Instant): Window = if (end?.isAfter(now) == false) Off else this

    fun records(now: Instant): Boolean = at(now) is Record

    fun spell(): String {
        fun iso(t: Instant) = t.truncatedTo(ChronoUnit.SECONDS).toString()
        return when (this) {
            Off -> "off"
            is Record -> "record ${iso(end)}"
            is Hold -> "hold ${iso(end)}"
        }
    }

    companion object {
        /** null for text that spells no window. */
        fun parse(text: String): Window? {
            val trimmed = text.trim()
            if (trimmed == "off") return Off
            val parts = trimmed.split(' ', limit = 2)
            if (parts.size != 2) return null
            val until = instant(parts[1]) ?: return null
            return when (parts[0]) {
                "record" -> Record(until)
                "hold" -> Hold(until)
                else -> null
            }
        }

        fun instant(text: String): Instant? =
            runCatching { OffsetDateTime.parse(text).toInstant() }.getOrNull()
    }
}

/** The next [hour]:[minute] after [now], in its zone: today, or tomorrow once today's
 * has passed, so "until 01:00" late at night means the coming night. */
fun nextTimeOfDay(hour: Int, minute: Int, now: ZonedDateTime): Instant {
    val today = now.toLocalDate().atTime(hour, minute).atZone(now.zone)
    return (if (today.isAfter(now)) today else today.plusDays(1)).toInstant()
}
