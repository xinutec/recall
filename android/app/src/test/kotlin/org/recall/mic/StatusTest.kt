package org.recall.mic

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Test

class StatusTest {
    private val host = "192.168.1.81"

    @Test
    fun aStreamingPhoneSaysWhereItsStreamGoes() {
        assertEquals(
            Status("Streaming", "to $host", Status.Tone.LIVE),
            Status.of(running = true, connected = true, quiet = false, host = host),
        )
        assertEquals(
            Status("Waiting for recall host", "trying $host…", Status.Tone.WAITING),
            Status.of(running = true, connected = false, quiet = false, host = host),
        )
    }

    @Test
    fun aFilesOnlyPhoneRecordsOnItselfAndNeverWaitsForAHost() {
        assertEquals(
            Status("Recording", "on this phone", Status.Tone.LIVE),
            Status.of(running = true, connected = true, quiet = false, host = ""),
        )
        val waiting = Status.of(running = true, connected = false, quiet = false, host = "")
        assertEquals(Status.Tone.WAITING, waiting.tone)
        assertFalse(waiting.label.contains("host") || waiting.detail.contains("host"))
    }

    @Test
    fun offAndStoppedReadTheSameWithOrWithoutAHost() {
        for (h in listOf(host, "")) {
            assertEquals(
                Status("Off", "no recording window open", Status.Tone.IDLE),
                Status.of(running = true, connected = false, quiet = true, host = h),
            )
            assertEquals(
                Status("Stopped", "not recording", Status.Tone.IDLE),
                Status.of(running = false, connected = false, quiet = false, host = h),
            )
        }
    }
}
