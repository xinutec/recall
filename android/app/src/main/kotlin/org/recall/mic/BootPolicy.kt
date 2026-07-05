package org.recall.mic

/** What BootReceiver should do after a reboot. */
enum class BootAction { AUTO_START, PROMPT, NOTHING }

/**
 * Pure decision for the after-boot restart, split out so it's unit-testable.
 *
 * Android 11+ (API 30) treats a microphone foreground service started from the
 * background as while-in-use restricted: it comes up but records SILENCE — and
 * Android 15 forbids the start outright (ForegroundServiceStartNotAllowedException,
 * a crash dialog). Hours of silent zeros that look like a live source in the
 * Devices panel are worse than a visibly dark mic. So from boot we auto-start only
 * where it actually records (API < 30) and otherwise post a tap-to-resume
 * notification — MainActivity's on-open resume path does the rest after one tap.
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
