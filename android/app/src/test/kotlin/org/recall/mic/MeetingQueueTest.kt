package org.recall.mic

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder
import java.io.File
import java.time.Instant
import java.time.ZoneId

class MeetingQueueTest {
    @get:Rule
    val tmp = TemporaryFolder()

    private val london = ZoneId.of("Europe/London")
    private val start = Instant.parse("2026-07-03T08:50:50Z") // 09:50:50 BST

    @Test
    fun namesARecordingByItsLocalStart() {
        assertEquals("meeting-20260703-095050.ogg", MeetingQueue.fileName(start, london))
    }

    @Test
    fun recoversTheStartFromTheFilename() {
        // The filename is the only record of when a recording was made, so it must
        // round-trip exactly — nothing else carries the start.
        assertEquals(
            start,
            MeetingQueue.startFromName(MeetingQueue.fileName(start, london), london),
        )
    }

    @Test
    fun ignoresNamesThatArentOurs() {
        assertNull(MeetingQueue.startFromName("2026_07_03_09_50_50_1.mp3", london))
        assertNull(MeetingQueue.startFromName("meeting-20261303-995050.ogg", london))
        assertNull(MeetingQueue.startFromName("recording.ogg", london))
    }

    @Test
    fun listsRecordingsOldestFirstWithTheirStarts() {
        val second = record("meeting-20260703-140000.ogg")
        val first = record("meeting-20260703-095050.ogg")

        val queue = MeetingQueue.list(tmp.root, london)
        assertEquals(listOf(first, second), queue.map { it.audio })
        assertEquals(start, queue[0].start)
    }

    @Test
    fun fallsBackToTheFileTimeForANameWeDidntWrite() {
        // Something copied into the directory by hand still shows up, at a plausible
        // time, rather than being silently hidden.
        val odd = record("interview.ogg").apply { setLastModified(1_770_000_000_000) }
        val queue = MeetingQueue.list(tmp.root, london)
        assertEquals(listOf(odd), queue.map { it.audio })
        assertEquals(Instant.ofEpochMilli(1_770_000_000_000), queue[0].start)
    }

    @Test
    fun skipsEmptyFilesAndNonRecordings() {
        // A MediaRecorder stopped before it wrote a page leaves a 0-byte file; posting it
        // would only earn a 400 from the server's ffprobe.
        File(tmp.root, "meeting-20260703-095050.ogg").createNewFile()
        record("notes.txt")
        assertTrue(MeetingQueue.list(tmp.root, london).isEmpty())
    }

    @Test
    fun deleteRemovesTheRecording() {
        val audio = record("meeting-20260703-095050.ogg")
        MeetingQueue.delete(MeetingQueue.list(tmp.root, london).single())
        assertFalse(audio.exists())
    }

    @Test
    fun movingARecordingIsHowItChangesState() {
        // Every state here is a directory, because a decision or a verdict has to survive
        // a reboot and a rename is the only change that can't half-happen.
        val audio = record("meeting-20260703-095050.ogg")
        val outbox = tmp.newFolder("outbox")

        val moved = MeetingQueue.moveTo(MeetingQueue.list(tmp.root, london).single(), outbox)

        assertEquals(File(outbox, audio.name), moved?.audio)
        assertFalse(audio.exists())
        // The name carries the start, so it survives the move intact.
        assertEquals(start, MeetingQueue.list(outbox, london).single().start)
    }

    @Test
    fun aShorterCopyOnTheHostIsNotAVerifiedUpload() {
        val tenMinutes = 600_000L
        // What a complete post looks like: the two probes disagree by milliseconds.
        assertFalse(MeetingQueue.landedShort(tenMinutes, tenMinutes))
        assertFalse(MeetingQueue.landedShort(tenMinutes, tenMinutes - 400))
        assertFalse(MeetingQueue.landedShort(tenMinutes, tenMinutes + 400))
        // A post cut short mid-stream still parses on the server, so this is the ONLY
        // signal that the phone holds the longer recording.
        assertTrue(MeetingQueue.landedShort(tenMinutes, tenMinutes - 30_000))
        assertTrue(MeetingQueue.landedShort(tenMinutes, 5_000))
    }

    @Test
    fun anUnknownLengthCountsAsUnverifiedNotAsAgreement() {
        // "Couldn't compare" must never read as "checked and fine" — the whole point is
        // whether the upload has been verified, and an unanswered question has not been.
        assertTrue(MeetingQueue.landedShort(0, 600_000))
        assertTrue(MeetingQueue.landedShort(600_000, 0))
        assertTrue(MeetingQueue.landedShort(0, 0))
    }

    @Test
    fun theOutboxIsNotListedAsARecording() {
        // The outbox is a subdirectory of the recordings directory; held recordings must
        // not gain a phantom row for it.
        record("meeting-20260703-095050.ogg")
        tmp.newFolder("outbox")
        assertEquals(1, MeetingQueue.list(tmp.root, london).size)
    }

    /** A file with some bytes in it, standing in for a recording. */
    private fun record(name: String): File =
        File(tmp.root, name).apply { writeBytes(ByteArray(64)) }
}
