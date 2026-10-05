package org.recall.web

import android.webkit.WebView
import org.xinutec.shell.ShellConfig
import org.xinutec.shell.WebShellActivity

/**
 * The recall web app at [RECALL_URL], in the fleet's shared [WebShellActivity]. Isis
 * serves it on the VPN, so it works at home and away.
 */
class MainActivity : WebShellActivity() {
    override val shell =
        ShellConfig(
            url = RECALL_URL,
            // The app and the Nextcloud sign-in, which must stay in the WebView or the
            // OAuth round trip cannot finish. Everything else opens in the browser.
            allowedHosts = setOf(RECALL_AUTHORITY, NC_HOST),
        )

    override fun onWebViewCreated(web: WebView) {
        // So audio plays on the first tap.
        web.settings.mediaPlaybackRequiresUserGesture = false
    }

    private companion object {
        const val RECALL_AUTHORITY = "recall.xinutec.org"
        const val RECALL_URL = "https://$RECALL_AUTHORITY/"

        // The Nextcloud sign-in. The shell never restores to its one-shot pages.
        const val NC_HOST = "dash.xinutec.org"
    }
}
