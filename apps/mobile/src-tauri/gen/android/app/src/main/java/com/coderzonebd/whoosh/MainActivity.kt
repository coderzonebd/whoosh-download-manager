// SPDX-License-Identifier: GPL-3.0-or-later
package com.coderzonebd.whoosh

import android.Manifest
import android.content.Intent
import android.content.pm.PackageManager
import android.graphics.Color
import android.net.Uri
import android.os.Build
import android.os.Bundle
import android.webkit.JavascriptInterface
import android.webkit.WebView
import androidx.activity.enableEdgeToEdge
import androidx.core.view.ViewCompat
import androidx.core.view.WindowCompat
import androidx.core.view.WindowInsetsCompat

/**
 * The app draws edge to edge, under the status and navigation bars. The web UI keeps clear of
 * them (and of the keyboard) through CSS variables this activity sets: --wdm-inset-top, -bottom,
 * -left, -right and -ime, in CSS pixels. The page also reads them once at start from
 * `WdmAndroid.insets()`, since a value pushed before it loaded is lost.
 *
 * `WdmAndroid.setBars(color)` lets the page say what colour sits under the bars, so their icons
 * stay readable (dark icons on a light app, light icons on a dark one).
 *
 * Android's back gesture is handled by WryActivity: it goes back in the webview's history, where
 * the UI keeps one entry per open sheet or page.
 *
 * Share → Whoosh Download Manager: the first web link in the shared text goes to the page
 * (`window.__wdmShared`), which opens the add dialog with it; one shared before the page was
 * ready is read with `WdmAndroid.takeShared()`. `WdmAndroid.openUrl(url)` opens an https page
 * (a new version's release page) in the browser.
 */
class MainActivity : TauriActivity() {
  private var webView: WebView? = null

  @Volatile private var insetsJson = "{}"

  @Volatile private var shared: String? = null

  override fun onCreate(savedInstanceState: Bundle?) {
    enableEdgeToEdge()
    super.onCreate(savedInstanceState)
    // Unpack yt-dlp's Python and ffmpeg while the UI loads (seconds, once per install).
    MediaTools.start(this)
    takeShare(intent)
    // The download notification (Android 13+): asked once; downloads work without it too.
    if (Build.VERSION.SDK_INT >= 33 &&
      checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) != PackageManager.PERMISSION_GRANTED
    ) {
      requestPermissions(arrayOf(Manifest.permission.POST_NOTIFICATIONS), 1)
    }
  }

  override fun onNewIntent(intent: Intent) {
    super.onNewIntent(intent)
    setIntent(intent)
    takeShare(intent)
    // The page takes it now if it is ready (it answers true); else it reads it with takeShared().
    shared?.let { url ->
      webView?.evaluateJavascript(
        "!!(window.__wdmShared && window.__wdmShared(${org.json.JSONObject.quote(url)}))",
      ) { taken -> if (taken == "true" && shared == url) shared = null }
    }
  }

  /** The first http(s) link in a shared text (a page's title often comes with it). */
  private fun takeShare(intent: Intent?) {
    if (intent?.action != Intent.ACTION_SEND) return
    val text = intent.getStringExtra(Intent.EXTRA_TEXT) ?: return
    shared = Regex("""https?://\S+""").find(text)?.value?.trimEnd('.', ',', ')', ']', '"', '\'')
  }

  override fun onWebViewCreate(webView: WebView) {
    this.webView = webView
    webView.addJavascriptInterface(Bridge(), "WdmAndroid")
    ViewCompat.setOnApplyWindowInsetsListener(webView) { _, insets ->
      val density = resources.displayMetrics.density
      val bars = insets.getInsets(WindowInsetsCompat.Type.systemBars() or WindowInsetsCompat.Type.displayCutout())
      val ime = insets.getInsets(WindowInsetsCompat.Type.ime())
      fun css(px: Int) = px / density
      insetsJson =
        "{\"top\":${css(bars.top)},\"bottom\":${css(bars.bottom)},\"left\":${css(bars.left)},\"right\":${css(bars.right)},\"ime\":${css(ime.bottom)}}"
      webView.evaluateJavascript("window.__wdmApplyInsets && window.__wdmApplyInsets($insetsJson)", null)
      insets
    }
  }

  private inner class Bridge {
    @JavascriptInterface
    fun insets(): String = insetsJson

    /** A link shared to the app before the page could take it; read once. */
    @JavascriptInterface
    fun takeShared(): String? {
      val url = shared
      shared = null
      return url
    }

    @JavascriptInterface
    fun openUrl(url: String) {
      if (!url.startsWith("https://")) return
      runOnUiThread {
        try {
          startActivity(Intent(Intent.ACTION_VIEW, Uri.parse(url)).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK))
        } catch (_: Exception) {
        }
      }
    }

    @JavascriptInterface
    fun setBars(color: String) {
      val parsed = try {
        Color.parseColor(color.trim())
      } catch (_: IllegalArgumentException) {
        return
      }
      val luminance = Color.luminance(parsed)
      runOnUiThread {
        val controller = WindowCompat.getInsetsController(window, window.decorView)
        controller.isAppearanceLightStatusBars = luminance > 0.5
        controller.isAppearanceLightNavigationBars = luminance > 0.5
      }
    }
  }
}
