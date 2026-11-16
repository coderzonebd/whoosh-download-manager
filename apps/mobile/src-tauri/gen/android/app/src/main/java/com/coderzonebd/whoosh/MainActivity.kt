// SPDX-License-Identifier: GPL-3.0-or-later
package com.coderzonebd.whoosh

import android.graphics.Color
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
 */
class MainActivity : TauriActivity() {
  private var webView: WebView? = null

  @Volatile private var insetsJson = "{}"

  override fun onCreate(savedInstanceState: Bundle?) {
    enableEdgeToEdge()
    super.onCreate(savedInstanceState)
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
