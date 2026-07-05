package org.recall.web

import android.annotation.SuppressLint
import android.app.Activity
import android.content.Context
import android.graphics.Color
import android.os.Bundle
import android.view.ViewGroup
import android.webkit.WebView
import android.webkit.WebViewClient
import android.widget.FrameLayout
import androidx.core.view.ViewCompat
import androidx.core.view.WindowInsetsCompat

/**
 * A full-screen [WebView] onto the recall web UI — the Angular SPA the recall API
 * serves at [RECALL_URL]. No address bar, no tabs, a home-screen icon: the website
 * presented as an app, avoiding browser chrome. LAN-only and cleartext (the host is
 * a private reserved address; see `usesCleartextTraffic` in the manifest).
 *
 * Deliberately tiny — a plain Activity holding one WebView, no Compose/AppCompat.
 * `configChanges` keeps the WebView (and its SPA route + scroll) across rotation
 * instead of recreating the activity.
 *
 * The WebView is inset from the system bars by padding a wrapper (see onCreate),
 * and the strips behind the bars are painted with the page's own surface colour.
 */
class MainActivity : Activity() {
    private lateinit var web: WebView
    private lateinit var root: FrameLayout

    @SuppressLint("SetJavaScriptEnabled")
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        val prefs = getSharedPreferences("viewer", Context.MODE_PRIVATE)
        web =
            WebView(this).apply {
                layoutParams =
                    ViewGroup.LayoutParams(
                        ViewGroup.LayoutParams.MATCH_PARENT,
                        ViewGroup.LayoutParams.MATCH_PARENT,
                    )
                settings.javaScriptEnabled = true // Angular needs JS
                settings.domStorageEnabled = true // localStorage / sessionStorage
                // Audio clips (/api/audio…) should play on tap without a prior gesture.
                settings.mediaPlaybackRequiresUserGesture = false
                settings.useWideViewPort = true
                settings.loadWithOverviewMode = true
                // Keep every navigation inside this WebView — never hand off to a
                // browser — and remember the current in-app page so a cold reopen
                // returns to it (SPA route changes fire doUpdateVisitedHistory too).
                webViewClient =
                    object : WebViewClient() {
                        override fun doUpdateVisitedHistory(
                            view: WebView,
                            url: String,
                            isReload: Boolean,
                        ) {
                            super.doUpdateVisitedHistory(view, url, isReload)
                            if (url.startsWith(RECALL_URL)) {
                                prefs.edit().putString(KEY_LAST_URL, url).apply()
                            }
                        }

                        // Paint the strips behind the system bars with the web UI's
                        // own surface colour instead of a hardcoded black; it follows
                        // the page's light/dark theme, so read its body background.
                        override fun onPageFinished(view: WebView, url: String) {
                            super.onPageFinished(view, url)
                            view.evaluateJavascript(
                                "getComputedStyle(document.body).backgroundColor",
                            ) { result -> parseCssColor(result)?.let(root::setBackgroundColor) }
                        }
                    }
                // Black until the page loads and reports its surface colour; avoids a
                // white flash on launch.
                setBackgroundColor(Color.BLACK)
            }
        // Inset the WebView from the system bars by padding a wrapper ViewGroup
        // (WebView.setPadding() doesn't offset content under wide-viewport mode).
        // Once the WebView no longer underlaps the bars its env(safe-area-inset-*)
        // collapse to 0, so the page's own safe-area CSS adds nothing on top.
        root =
            FrameLayout(this).apply {
                addView(web)
                setBackgroundColor(Color.BLACK)
            }
        ViewCompat.setOnApplyWindowInsetsListener(root) { v, insets ->
            val bars = insets.getInsets(WindowInsetsCompat.Type.systemBars())
            v.setPadding(bars.left, bars.top, bars.right, bars.bottom)
            WindowInsetsCompat.CONSUMED
        }
        setContentView(root)
        // Reopen where we left off; the hardcoded URL is only the first-run default.
        web.loadUrl(prefs.getString(KEY_LAST_URL, null) ?: RECALL_URL)
    }

    // The hardware/gesture Back walks the SPA's history; it only leaves the app once
    // there's nothing left to go back to.
    @Deprecated("Deprecated in Java")
    @Suppress("DEPRECATION")
    override fun onBackPressed() {
        if (web.canGoBack()) web.goBack() else super.onBackPressed()
    }

    // evaluateJavascript hands back the JSON-encoded result, e.g. the string
    // "rgb(18, 18, 18)" (with quotes) or "rgba(18, 18, 18, 1)". Pull out the RGB
    // triple; alpha is ignored (the surface is opaque). null if it can't be read.
    private fun parseCssColor(raw: String?): Int? {
        val m = raw?.let { Regex("""rgba?\((\d+),\s*(\d+),\s*(\d+)""").find(it) } ?: return null
        val (r, g, b) = m.destructured
        return Color.rgb(r.toInt(), g.toInt(), b.toInt())
    }

    companion object {
        // mac-mini's DHCP-reserved LAN address (192.168.1.81); the recall API serves
        // the built Angular UI here on :8000. Hardcoded — this app is single-purpose.
        private const val RECALL_URL = "http://192.168.1.81:8000/"
        private const val KEY_LAST_URL = "last_url"
    }
}
