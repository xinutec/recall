package org.recall.mic

import java.util.Locale

/**
 * The line sent on the shared ingest port before any PCM: the device, the stream's
 * shape, and `epochMillis`, the wall clock just after `startRecording()`, which the
 * server stamps the segments with so all mics share a clock.
 *
 * The epoch is fixed-point seconds (a Double could print in scientific notation), in
 * Locale.ROOT so the decimal point is a point.
 */
fun handshakeLine(deviceId: String, sampleRate: Int, epochMillis: Long): String {
    val epoch = String.format(Locale.ROOT, "%d.%03d", epochMillis / 1000, epochMillis % 1000)
    return """{"id":"$deviceId","rate":$sampleRate,"channels":1,"epoch":$epoch}""" + "\n"
}
