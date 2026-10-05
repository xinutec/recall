package org.recall.mic

/**
 * Every notification id the app posts, together, so two cannot share one: three once
 * shared id 2, each silently replacing the others (#1201).
 */
object NotificationIds {
    /** [StreamService]'s ongoing capture notification. */
    const val STREAM = 1

    /** [MeetingService]'s, while it records. */
    const val MEETING = 2

    /** [ResumeWarningReceiver]'s "recording resumes soon". */
    const val RESUME_WARNING = 3

    /** [BootReceiver]'s "stopped by reboot" prompt. */
    const val BOOT = 4
}
