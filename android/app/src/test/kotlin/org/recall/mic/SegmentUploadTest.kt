package org.recall.mic

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class SegmentUploadTest {
    private val bytes = "pretend audio".toByteArray()
    private val sha = SegmentUpload.sha256Hex(bytes)

    @Test
    fun `a receipt counts only when it names our hash and our byte count`() {
        assertTrue(
            SegmentUpload.receiptMatches(
                """{"sha256":"$sha","bytes":${bytes.size}}""",
                sha,
                bytes.size,
            ),
        )
    }

    @Test
    fun `a 2xx with a foreign hash is not a delivery`() {
        val foreign = "0".repeat(64)
        assertFalse(
            SegmentUpload.receiptMatches(
                """{"sha256":"$foreign","bytes":${bytes.size}}""",
                sha,
                bytes.size,
            ),
        )
    }

    @Test
    fun `a short store is not a delivery even with a matching hash field`() {
        assertFalse(
            SegmentUpload.receiptMatches(
                """{"sha256":"$sha","bytes":${bytes.size - 1}}""",
                sha,
                bytes.size,
            ),
        )
    }

    @Test
    fun `garbage in place of a receipt is not a delivery`() {
        assertFalse(SegmentUpload.receiptMatches("<html>proxy error</html>", sha, bytes.size))
        assertFalse(SegmentUpload.receiptMatches("", sha, bytes.size))
    }

    @Test
    fun `the local digest is a real sha-256`() {
        // An independently computed vector, so the digest is proven, not assumed.
        assertEquals(
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824",
            SegmentUpload.sha256Hex("hello".toByteArray()),
        )
    }
}
