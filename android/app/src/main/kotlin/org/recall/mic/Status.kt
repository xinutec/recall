package org.recall.mic

/** What the status card says: its title, the line under it, and its tone. */
internal data class Status(
    val label: String,
    val detail: String,
    val tone: Tone,
) {
    enum class Tone { LIVE, WAITING, IDLE }

    companion object {
        /**
         * `connected` is the service's "streaming", which a files-only phone ([FilesOnly])
         * sets while it records. `quiet`: recall's window keeps the mic off.
         */
        fun of(running: Boolean, connected: Boolean, quiet: Boolean, host: String): Status {
            val filesOnly = FilesOnly.applies(host)
            return when {
                connected && filesOnly -> Status("Recording", "on this phone", Tone.LIVE)

                connected -> Status("Streaming", "to $host", Tone.LIVE)

                // Off closes the host's listener; not an error.
                running && quiet -> Status("Off", "no recording window open", Tone.IDLE)

                running && filesOnly -> Status("Waiting", "to record on this phone", Tone.WAITING)

                running -> Status("Waiting for recall host", "trying $host…", Tone.WAITING)

                else -> Status("Stopped", "not recording", Tone.IDLE)
            }
        }
    }
}
