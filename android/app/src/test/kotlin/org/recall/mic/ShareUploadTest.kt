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
    fun sendsATitlePartOnlyWhenThereIsATitle() {
        assertEquals(
            "--B\r\nContent-Disposition: form-data; name=\"title\"\r\n\r\nOncology clinic\r\n",
            ShareUpload.titlePart("B", "  Oncology clinic  "),
        )
        // Blank means "let the server name it" — an empty title part would override the
        // server's `Meeting <date> <time>` with nothing at all.
        assertEquals("", ShareUpload.titlePart("B", ""))
        assertEquals("", ShareUpload.titlePart("B", "   "))
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
