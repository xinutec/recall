package org.recall.mic

/**
 * The base URL of a recall API. A value with a scheme is used as written (the fleet,
 * `https://recall.xinutec.org`); a bare host is `http://<host>:8000`, the shape the
 * Mac's LAN beat relay answers.
 */
object ApiBase {
    private const val RELAY_PORT = 8000 // `recall beat-relay`, the LAN backstop

    fun of(hostOrUrl: String): String =
        if ("://" in hostOrUrl) hostOrUrl.trimEnd('/') else "http://$hostOrUrl:$RELAY_PORT"
}
