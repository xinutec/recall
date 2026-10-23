package org.recall.mic

import java.time.Instant

/**
 * A phone with no recorder host records one-minute files on its own, wherever it is,
 * and [SegmentUpload] delivers them: a person's own phone, with no house mics to stream
 * to. It follows its owner's recording window from recall, asked with its own token,
 * and keeps the last one it heard (spelled, in [Prefs]), so out of reach, or after a
 * restart, it records until that window's end and no longer.
 */
internal object FilesOnly {
    /** Files only when no recorder host is set. */
    fun applies(host: String): Boolean = host.isBlank()

    /** The window to keep after a poll: recall's answer when there is one, else the
     * last known. A fault in the answer is kept as blank, which reads as none. */
    fun kept(saved: String?, answer: CaptureState?): String? =
        answer?.let { it.desiredWindow()?.spell().orEmpty() } ?: saved

    /** Whether to record at [now]. No window known, or one that does not read, is a
     * fault, and records. */
    fun records(saved: String?, now: Instant): Boolean =
        saved?.let(Window::parse)?.records(now) ?: true
}
