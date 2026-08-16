package org.recall.mic

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test
import java.time.Instant
import java.time.ZoneId

class ShareUploadTest {
    private val london = ZoneId.of("Europe/London")

    @Test
    fun parsesTheRecorderTimestampFromTheFilename() {
        // The mp3 recorder names files with the local start, e.g. this hospital clip.
        // 09:50:50 BST (London) → 08:50:50 UTC.
        assertEquals(
            Instant.parse("2026-07-03T08:50:50Z"),
            ShareUpload.parseRecorderStart("2026_07_03_09_50_50_1.mp3", london),
        )
    }

    @Test
    fun ignoresAFilenameWithoutARecorderStamp() {
        assertNull(ShareUpload.parseRecorderStart("voice-memo.mp3", london))
        assertNull(ShareUpload.parseRecorderStart("2026_13_40_99_99_99.mp3", london))
    }

    @Test
    fun choosesTheRecorderStampOverEverythingElse() {
        val now = Instant.parse("2026-07-03T20:00:00Z")
        val modified = Instant.parse("2026-07-03T19:00:00Z").toEpochMilli()
        assertEquals(
            Instant.parse("2026-07-03T08:50:50Z"),
            ShareUpload.chooseStart("2026_07_03_09_50_50_1.mp3", modified, now, london),
        )
    }

    @Test
    fun readsTheDeliveredLengthFromTheResponse() {
        // The receipt: what recall says it actually received, which is what makes a 2xx
        // checkable rather than merely reassuring.
        val body =
            """{"id":"meeting-20260703-0950","title":"Meeting 2026-07-03 09:50",""" +
                """"start":"2026-07-03T08:50:50+00:00","end":"2026-07-03T09:35:50+00:00",""" +
                """"turnCount":0,"speakers":[]}"""
        assertEquals(45 * 60 * 1000L, ShareUpload.sessionDurationMs(body))
    }

    @Test
    fun reportsNoLengthRatherThanGuessingOne() {
        // An older server, an error body, or junk: 0, which the caller reads as
        // "not verified" — never as agreement.
        assertEquals(0L, ShareUpload.sessionDurationMs("""{"title":"x"}"""))
        assertEquals(0L, ShareUpload.sessionDurationMs("not json"))
    }

    @Test
    fun readsMediaStoreSecondsAsSecondsAndSafMillisAsMillis() {
        // The two providers disagree on units. Seconds read as millis would date a 2026
        // recording to 1970 — and unlike the `now` fallback, that failure LOOKS like the
        // file's own time was honoured, which is why it gets its own test.
        val millis = Instant.parse("2026-07-03T19:00:00Z").toEpochMilli()
        assertEquals(millis, ShareUpload.modifiedMillis(millis, null))
        assertEquals(millis, ShareUpload.modifiedMillis(null, millis / 1000))
        // SAF wins when both are offered; both are the same moment, in their own units.
        assertEquals(millis, ShareUpload.modifiedMillis(millis, millis / 1000))
    }

    @Test
    fun treatsAnAbsentTimestampAsAbsentRatherThanTheEpoch() {
        assertNull(ShareUpload.modifiedMillis(null, null))
        assertNull(ShareUpload.modifiedMillis(0, 0))
        assertNull(ShareUpload.modifiedMillis(-1, null))
    }

    @Test
    fun fallsBackToLastModifiedThenNow() {
        val now = Instant.parse("2026-07-03T20:00:00Z")
        val modified = Instant.parse("2026-07-03T19:00:00Z").toEpochMilli()
        assertEquals(
            Instant.ofEpochMilli(modified),
            ShareUpload.chooseStart("memo.mp3", modified, now, london),
        )
        assertEquals(now, ShareUpload.chooseStart("memo.mp3", null, now, london))
        assertEquals(now, ShareUpload.chooseStart("memo.mp3", 0, now, london))
    }
}
