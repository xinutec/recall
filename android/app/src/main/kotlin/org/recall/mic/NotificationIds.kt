package org.recall.mic

/**
 * Every notification id this app posts, in one readable set.
 *
 * Not a private constant per component, which is what it was: the meeting recorder, the
 * boot notice and the resume warning all ended up on id 2, where whichever posted last
 * silently took the slot from the others (#1201). Nothing at a call site can show that —
 * `notify(NOTIFICATION_ID, …)` reads correctly in each file on its own — so the ids are
 * only checkable when they sit together.
 *
 * The failure it causes reads as the wrong thing, too: a notification that was replaced
 * looks exactly like one that was never posted, which sends you looking at the code that
 * posts rather than at the code that posted over it.
 */
object NotificationIds {
    /** [StreamService]'s ongoing "streaming / paused / waiting" capture notification. */
    const val STREAM = 1

    /** [MeetingService]'s ongoing notification while it records a meeting to a file. */
    const val MEETING = 2

    /** [ResumeWarningReceiver]'s "recording resumes soon" heads-up, taken back down by
     *  [ResumeWarning] when the pause moves. */
    const val RESUME_WARNING = 3

    /** [BootReceiver]'s "stopped by reboot — tap to resume streaming" prompt. Its own id
     *  since a meeting started after a reboot used to post over the prompt, so the one
     *  thing asking for streaming to be restarted disappeared unacted on. */
    const val BOOT = 4
}
