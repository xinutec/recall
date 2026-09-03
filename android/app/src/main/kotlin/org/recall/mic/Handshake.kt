package org.recall.mic

import java.util.Locale

// The ingest handshake line — pure string building, so it's unit-tested on the
// JVM (the streaming service just calls it).

/**
 * The one-line announcement sent on the shared ingest port before any PCM:
 * identity, stream shape, and the capture epoch of the first PCM byte.
 *
 * `epochMillis` is the phone's wall-clock right after AudioRecord.startRecording()
 * — the capture instant of the earliest samples the first read will drain. The
 * server measures that byte's arrival and renames this connection's segments from
 * arrival time back to capture time, so cross-mic timestamps share a clock.
 * Rendered as fixed-point seconds (never Double interpolation, which turns large
 * values into scientific notation), Locale.ROOT so the decimal point survives
 * every locale.
 */
fun handshakeLine(deviceId: String, sampleRate: Int, epochMillis: Long): String {
    val epoch = String.format(Locale.ROOT, "%d.%03d", epochMillis / 1000, epochMillis % 1000)
    return """{"id":"$deviceId","rate":$sampleRate,"channels":1,"epoch":$epoch}""" + "\n"
}
