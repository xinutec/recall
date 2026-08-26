package org.recall.mic

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class NotificationIdsTest {
    @Test
    fun everyNotificationIdIsDistinct() {
        // Read by reflection rather than from a list written here: the failure being
        // guarded against is a new id copy-pasted from an old one, and a hand-kept list
        // would be updated by the same hand that copied. This way a constant added to
        // NotificationIds is in the check without anyone remembering to add it.
        val ids = declaredIds()
        // Without this, an empty read (fields renamed, the object made non-const) would
        // satisfy the distinctness assertion below by having nothing to compare.
        assertTrue("no id constants read from NotificationIds", ids.size >= 4)
        assertEquals("notification ids must be distinct", ids.size, ids.distinct().size)
    }

    @Test
    fun theStreamKeepsIdOneSoAnUpgradeReplacesItsOngoingNotification() {
        // The capture service's notification is ongoing across an upgrade. Renumbering it
        // would leave the old one stranded in the shade with no service behind it, so this
        // one id is not free to move.
        assertEquals(1, NotificationIds.STREAM)
    }

    private fun declaredIds(): List<Int> =
        NotificationIds::class.java.declaredFields
            .filter { it.type == Int::class.javaPrimitiveType }
            .map {
                it.isAccessible = true
                it.getInt(NotificationIds)
            }
}
