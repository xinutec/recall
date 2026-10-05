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
        // Nothing else records the start.
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
        // As for a file copied in by hand.
        val odd = record("interview.ogg").apply { setLastModified(1_770_000_000_000) }
        val queue = MeetingQueue.list(tmp.root, london)
        assertEquals(listOf(odd), queue.map { it.audio })
        assertEquals(Instant.ofEpochMilli(1_770_000_000_000), queue[0].start)
    }

    @Test
    fun skipsEmptyFilesAndNonRecordings() {
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
        val audio = record("meeting-20260703-095050.ogg")
        val outbox = tmp.newFolder("outbox")

        val moved = MeetingQueue.moveTo(MeetingQueue.list(tmp.root, london).single(), outbox)

        assertEquals(File(outbox, audio.name), moved?.audio)
        assertFalse(audio.exists())
        assertEquals(start, MeetingQueue.list(outbox, london).single().start)
    }

    @Test
    fun aShorterCopyOnTheHostIsNotAVerifiedUpload() {
        val tenMinutes = 600_000L
        // A complete post: the two probes differ by milliseconds.
        assertFalse(MeetingQueue.landedShort(tenMinutes, tenMinutes))
        assertFalse(MeetingQueue.landedShort(tenMinutes, tenMinutes - 400))
        assertFalse(MeetingQueue.landedShort(tenMinutes, tenMinutes + 400))
        // A post cut short.
        assertTrue(MeetingQueue.landedShort(tenMinutes, tenMinutes - 30_000))
        assertTrue(MeetingQueue.landedShort(tenMinutes, 5_000))
    }

    @Test
    fun anUnknownLengthCountsAsUnverifiedNotAsAgreement() {
        assertTrue(MeetingQueue.landedShort(0, 600_000))
        assertTrue(MeetingQueue.landedShort(600_000, 0))
        assertTrue(MeetingQueue.landedShort(0, 0))
    }

    @Test
    fun theOutboxIsNotListedAsARecording() {
        // A subdirectory of the recordings directory.
        record("meeting-20260703-095050.ogg")
        tmp.newFolder("outbox")
        assertEquals(1, MeetingQueue.list(tmp.root, london).size)
    }

    @Test
    fun aFailureIsRememberedBesideTheRecording() {
        val audio = record("meeting-20260703-095050.ogg")
        assertNull(MeetingQueue.failure(audio))

        MeetingQueue.noteFailure(audio, "Not authorised — check the upload token.")
        assertEquals("Not authorised — check the upload token.", MeetingQueue.failure(audio))

        MeetingQueue.clearFailure(audio)
        assertNull(MeetingQueue.failure(audio))
    }

    @Test
    fun aFailureNoteIsNeverListedAsARecording() {
        // Or the uploader would post it.
        val audio = record("meeting-20260703-095050.ogg")
        MeetingQueue.noteFailure(audio, "Upload failed.")
        assertEquals(listOf(audio), MeetingQueue.list(tmp.root, london).map { it.audio })
    }

    @Test
    fun deliveryTakesTheFailureNoteWithIt() {
        val audio = record("meeting-20260703-095050.ogg")
        MeetingQueue.noteFailure(audio, "Not authorised — check the upload token.")
        val uploaded = tmp.newFolder("uploaded")

        MeetingQueue.moveTo(MeetingQueue.list(tmp.root, london).single(), uploaded)

        assertNull(MeetingQueue.failure(audio))
        assertFalse(File(tmp.root, audio.name + MeetingQueue.FAILURE_SUFFIX).exists())
    }

    @Test
    fun deletingARecordingTakesItsFailureNoteToo() {
        val audio = record("meeting-20260703-095050.ogg")
        MeetingQueue.noteFailure(audio, "Upload failed.")
        MeetingQueue.delete(MeetingQueue.list(tmp.root, london).single())
        assertFalse(File(tmp.root, audio.name + MeetingQueue.FAILURE_SUFFIX).exists())
    }

    @Test
    fun theOutboxStateIsWhatTheFleetIsTold() {
        val old = record("meeting-20260703-095050.ogg")
        record("meeting-20260703-140000.ogg")
        MeetingQueue.noteFailure(old, "Not authorised — check the upload token.")

        val state = MeetingQueue.state(tmp.root, london)

        assertEquals(2, state.queued)
        assertEquals(start, state.oldestStart)
        assertEquals(1, state.failing)
        assertEquals("Not authorised — check the upload token.", state.reason)
    }

    @Test
    fun anEmptyOutboxIsStillAReportableState() {
        // So the server's check can return to green.
        val state = MeetingQueue.state(tmp.root, london)
        assertEquals(0, state.queued)
        assertEquals(0, state.failing)
        assertNull(state.oldestStart)
        assertNull(state.reason)
    }

    /** A non-empty file standing in for a recording. */
    private fun record(name: String): File =
        File(tmp.root, name).apply { writeBytes(ByteArray(64)) }
}
