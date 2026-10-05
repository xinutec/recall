package org.recall.mic

/** What BootReceiver should do after a reboot. */
enum class BootAction { AUTO_START, PROMPT, NOTHING }

/**
 * What to do after boot. From Android 11 a mic service started in the background
 * records silence, which would look like a live source; Android 15 refuses the start.
 * So it starts by itself only below Android 11, and otherwise asks for a tap.
 */
object BootPolicy {
    private const val WHILE_IN_USE_SDK = 30 // Android 11

    fun decide(
        sdkInt: Int,
        enabled: Boolean,
        hostSet: Boolean,
        hasMicPermission: Boolean,
    ): BootAction =
        when {
            !enabled || !hostSet -> BootAction.NOTHING
            !hasMicPermission -> BootAction.PROMPT
            sdkInt >= WHILE_IN_USE_SDK -> BootAction.PROMPT
            else -> BootAction.AUTO_START
        }
}
