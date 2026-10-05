package org.recall.mic

import org.junit.Assert.assertEquals
import org.junit.Test

class BootPolicyTest {
    @Test
    fun disabledOrUnconfiguredDoesNothing() {
        assertEquals(
            BootAction.NOTHING,
            BootPolicy.decide(
                sdkInt = 29,
                enabled = false,
                hostSet = true,
                hasMicPermission = true,
            ),
        )
        assertEquals(
            BootAction.NOTHING,
            BootPolicy.decide(
                sdkInt = 29,
                enabled = true,
                hostSet = false,
                hasMicPermission = true,
            ),
        )
    }

    @Test
    fun modernAndroidPromptsInsteadOfStreamingSilence() {
        // See BootPolicy.
        assertEquals(
            BootAction.PROMPT,
            BootPolicy.decide(sdkInt = 30, enabled = true, hostSet = true, hasMicPermission = true),
        )
        assertEquals(
            BootAction.PROMPT,
            BootPolicy.decide(sdkInt = 36, enabled = true, hostSet = true, hasMicPermission = true),
        )
    }

    @Test
    fun oldAndroidAutoStartsWhenPermitted() {
        assertEquals(
            BootAction.AUTO_START,
            BootPolicy.decide(sdkInt = 29, enabled = true, hostSet = true, hasMicPermission = true),
        )
    }

    @Test
    fun missingMicPermissionPromptsRatherThanCrashing() {
        assertEquals(
            BootAction.PROMPT,
            BootPolicy.decide(
                sdkInt = 29,
                enabled = true,
                hostSet = true,
                hasMicPermission = false,
            ),
        )
    }
}
