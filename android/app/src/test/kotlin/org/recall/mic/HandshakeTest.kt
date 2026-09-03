package org.recall.mic

import org.junit.Assert.assertEquals
import org.junit.Test

class HandshakeTest {
    @Test
    fun carriesIdRateChannelsAndEpoch() {
        assertEquals(
            """{"id":"pixel5","rate":48000,"channels":1,"epoch":1756900000.250}""" + "\n",
            handshakeLine("pixel5", 48000, epochMillis = 1_756_900_000_250L),
        )
    }

    @Test
    fun epochIsPlainDecimalNeverScientificNotation() {
        // Double interpolation would render 1.7569E9; the server would still parse
        // it, but a plain fixed-point millis rendering is exact and unambiguous.
        val line = handshakeLine("pixel5", 48000, epochMillis = 1_756_900_000_007L)
        assert("E" !in line) { line } // Double.toString would render 1.756...E9
        assert(""""epoch":1756900000.007""" in line) { line }
    }
}
