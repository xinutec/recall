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
        // The filename is the fallback when the sidecar is gone, so it must round-trip.
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
    fun roundTripsTheSidecar() {
        val audio = record("meeting-20260703-095050.ogg")
        MeetingQueue.writeSidecar(audio, "Oncology clinic", start)
        assertEquals("Oncology clinic" to start, MeetingQueue.readSidecar(audio))
    }

    @Test
    fun readsNothingWhenThereIsNoSidecar() {
        assertNull(MeetingQueue.readSidecar(record("meeting-20260703-095050.ogg")))
    }

    @Test
    fun listsPendingRecordingsOldestFirst() {
        val second = record("meeting-20260703-140000.ogg")
        val first = record("meeting-20260703-095050.ogg")
        MeetingQueue.writeSidecar(first, "Clinic", start)
        MeetingQueue.writeSidecar(second, "", Instant.parse("2026-07-03T13:00:00Z"))

        val queue = MeetingQueue.list(tmp.root, london)
        assertEquals(listOf(first, second), queue.map { it.audio })
        assertEquals(listOf("Clinic", ""), queue.map { it.title })
        assertEquals(start, queue[0].start)
    }

    @Test
    fun fallsBackToTheFilenameWhenTheSidecarIsMissing() {
        // The crash case: audio written, sidecar lost. The recording must still upload,
        // at the right time, rather than be stranded.
        record("meeting-20260703-095050.ogg")
        val queue = MeetingQueue.list(tmp.root, london)
        assertEquals(1, queue.size)
        assertEquals(start, queue[0].start)
        assertEquals("", queue[0].title)
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
    fun completeRemovesTheRecordingAndItsSidecar() {
        val audio = record("meeting-20260703-095050.ogg")
        MeetingQueue.writeSidecar(audio, "Clinic", start)
        val sidecar = File(audio.path + ".json")
        assertTrue(sidecar.exists())

        MeetingQueue.complete(MeetingQueue.list(tmp.root, london).single())
        assertFalse(audio.exists())
        assertFalse(sidecar.exists())
    }

    @Test
    fun approveMovesARecordingAndItsMetadataIntoTheOutbox() {
        // Approval is the only thing that makes a recording uploadable, so it has to be a
        // fact on disk — not a flag that a reboot forgets.
        val audio = record("meeting-20260703-095050.ogg")
        MeetingQueue.writeSidecar(audio, "Clinic", start)
        val outbox = tmp.newFolder("outbox")

        val moved = MeetingQueue.approve(MeetingQueue.list(tmp.root, london).single(), outbox)

        assertEquals(File(outbox, audio.name), moved?.audio)
        assertFalse(audio.exists())
        assertFalse(File(audio.path + ".json").exists())
        // And it arrives whole: title and start survive the move, so it doesn't upload
        // under a recovered name.
        val queued = MeetingQueue.list(outbox, london).single()
        assertEquals("Clinic", queued.title)
        assertEquals(start, queued.start)
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
