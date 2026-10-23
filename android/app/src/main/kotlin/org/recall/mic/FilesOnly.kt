package org.recall.mic

/**
 * A phone with no recorder host records one-minute files on its own, wherever it is,
 * and [SegmentUpload] delivers them: a person's own phone, with no house mics to stream
 * to. It follows its owner's pause from recall, asked with its own token.
 */
internal object FilesOnly {
    /** Files only when no recorder host is set. */
    fun applies(host: String): Boolean = host.isBlank()

    /** Whether to stay paused: recall's answer when there is one, else the last known,
     * so a phone out of reach keeps recording unless it was paused. */
    fun paused(last: Boolean, answer: CaptureState?): Boolean =
        answer?.let { !it.desiredRunning } ?: last
}
