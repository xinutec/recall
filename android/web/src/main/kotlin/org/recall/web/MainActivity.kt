package org.recall.web

import android.webkit.WebView
import org.xinutec.shell.ShellConfig
import org.xinutec.shell.WebShellActivity

/**
 * The recall web UI — the Angular SPA the recall API serves at [RECALL_URL], in the
 * fleet's shared [WebShellActivity]. Points at Isis over WireGuard, so it works on
 * the VPN whether home or away; cleartext is fine over the encrypted tunnel to a
 * private address (see `usesCleartextTraffic` in the manifest).
 *
 * The eighth wrapper, and the one that shows why they are shared: while it kept its
 * own copy it had drifted to insetting for `systemBars()` alone, so the keyboard
 * drew straight over the page — the exact bug the other seven had already fixed.
 */
class MainActivity : WebShellActivity() {
    override val shell =
        ShellConfig(
            url = RECALL_URL,
            // The UI plus the Nextcloud login hop — without the second, the OAuth
            // round-trip would be ejected to the browser and the app could never
            // sign in again. Everything else opens in the real browser.
            // The port is part of it: isis runs several of the fleet's services, and
            // a host-only rule would let any of them open in place.
            allowedHosts = setOf(RECALL_AUTHORITY, NC_HOST),
        )

    override fun onWebViewCreated(web: WebView) {
        // Audio clips (/api/audio…) should play on tap without a prior gesture.
        web.settings.mediaPlaybackRequiresUserGesture = false
    }

    private companion object {
        // recalld on Isis, the fleet system of record, at a name Isis's front door serves
        // on the VPN only, so it works whether home or away. Pause/resume on this UI
        // drives the capture intent the Mac mirrors. Hardcoded — this app is
        // single-purpose.
        const val RECALL_AUTHORITY = "recall.xinutec.org"
        const val RECALL_URL = "https://$RECALL_AUTHORITY/"

        // The Nextcloud identity provider the sign-in bounces through. Its one-shot
        // /login and /auth/callback hops are never restore points — the shell's
        // Restore filters them, which is what this app's own isAuthFlowUrl did.
        const val NC_HOST = "dash.xinutec.org"
    }
}
