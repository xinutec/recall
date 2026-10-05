package org.recall.mic

import org.junit.Assert.assertEquals
import org.junit.Test

/** A host setting is either a URL (Isis, #1799) or a bare host (the Mac's relay on :8000). */
class ApiBaseTest {
    @Test
    fun `a value with a scheme is used as written`() {
        assertEquals("https://recall.xinutec.org", ApiBase.of("https://recall.xinutec.org"))
        assertEquals("https://recall.xinutec.org", ApiBase.of("https://recall.xinutec.org/"))
    }

    @Test
    fun `a bare host is the relay shape on 8000`() {
        assertEquals("http://192.168.1.20:8000", ApiBase.of("192.168.1.20"))
    }

    @Test
    fun `an unset or old default control setting becomes the fleet name`() {
        assertEquals(Prefs.DEFAULT_CONTROL_HOST, Prefs.effectiveControlHost(""))
        assertEquals(Prefs.DEFAULT_CONTROL_HOST, Prefs.effectiveControlHost("10.100.0.2"))
        assertEquals("https://recall.xinutec.org", Prefs.DEFAULT_CONTROL_HOST)
    }

    @Test
    fun `a host somebody chose is kept`() {
        assertEquals("192.168.1.20", Prefs.effectiveControlHost("192.168.1.20"))
    }
}
