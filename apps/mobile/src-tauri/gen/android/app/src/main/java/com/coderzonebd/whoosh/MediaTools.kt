// SPDX-License-Identifier: GPL-3.0-or-later
package com.coderzonebd.whoosh

import android.content.Context
import android.util.Log
import app.tauri.plugin.JSObject
import com.yausername.ffmpeg.FFmpeg
import com.yausername.youtubedl_android.YoutubeDL
import java.io.File
import kotlin.concurrent.thread

/**
 * yt-dlp, ffmpeg and QuickJS for video and audio sites, from youtubedl-android: Python, ffmpeg
 * and QuickJS are in the app's native library directory (the only place Android 10+ lets an app
 * run programs from, hence the `lib*.so` names); their libraries are unpacked on the first start
 * after an install (about 40 MB, a few seconds). The engine (Rust) runs the programs itself with
 * the paths and environment [info] gives it (`WdmPlugin.mediaTools`).
 *
 * yt-dlp itself is a file Python runs; it is updated from yt-dlp's GitHub releases at most once
 * a day, because sites change faster than app releases.
 */
object MediaTools {
  private const val TAG = "WdmMediaTools"
  private const val BASE = "youtubedl-android" // the library's own directory names
  private const val UPDATE_EVERY_MS = 24L * 60 * 60 * 1000

  private val done = Object()
  @Volatile private var started = false
  @Volatile private var finished = false
  @Volatile private var result: JSObject? = null
  @Volatile var error: String? = null
    private set

  /** Unpacks the tools in the background; safe to call more than once. */
  fun start(context: Context) {
    synchronized(this) {
      if (started) return
      started = true
    }
    val app = context.applicationContext
    thread(name = "wdm-media-tools", isDaemon = true) {
      val t0 = System.currentTimeMillis()
      try {
        YoutubeDL.getInstance().init(app)
        FFmpeg.getInstance().init(app)
        result = info(app)
        Log.i(TAG, "media tools ready in ${System.currentTimeMillis() - t0} ms")
      } catch (e: Throwable) {
        Log.w(TAG, "media tools unavailable", e)
        // The whole chain: an ExceptionInInitializerError says nothing by itself.
        error = Log.getStackTraceString(e).lines().take(40).joinToString("\n")
      }
      synchronized(done) {
        finished = true
        done.notifyAll()
      }
      if (result != null) updateIfDue(app)
    }
  }

  /** The tools once unpacked, or null (failed, or not within [timeoutMs]). */
  fun await(timeoutMs: Long): JSObject? {
    val until = System.currentTimeMillis() + timeoutMs
    synchronized(done) {
      while (!finished) {
        val left = until - System.currentTimeMillis()
        if (left <= 0) return null
        done.wait(left)
      }
    }
    return result
  }

  private fun info(app: Context): JSObject {
    val bin = File(app.applicationInfo.nativeLibraryDir)
    val base = File(app.noBackupFilesDir, BASE)
    val packages = File(base, "packages")
    val pythonHome = File(packages, "python/usr").absolutePath
    val env = JSObject()
    env.put(
      "LD_LIBRARY_PATH",
      listOf("python", "ffmpeg", "aria2c").joinToString(":") { File(packages, "$it/usr/lib").absolutePath },
    )
    env.put("SSL_CERT_FILE", "$pythonHome/etc/tls/cert.pem")
    env.put("PYTHONHOME", pythonHome)
    env.put("HOME", pythonHome)
    env.put("TMPDIR", app.cacheDir.absolutePath)
    env.put("PATH", (System.getenv("PATH") ?: "/system/bin") + ":" + bin.absolutePath)
    val out = JSObject()
    out.put("python", File(bin, "libpython.so").absolutePath)
    out.put("ytDlp", File(base, "yt-dlp/yt-dlp").absolutePath)
    out.put("ffmpeg", File(bin, "libffmpeg.so").absolutePath)
    out.put("quickjs", File(bin, "libqjs.so").absolutePath)
    out.put("env", env)
    return out
  }

  private fun updateIfDue(app: Context) {
    val prefs = app.getSharedPreferences("wdm-media-tools", Context.MODE_PRIVATE)
    val last = prefs.getLong("ytDlpCheckedAt", 0)
    val now = System.currentTimeMillis()
    if (now - last < UPDATE_EVERY_MS) return
    try {
      val status = YoutubeDL.getInstance().updateYoutubeDL(app, YoutubeDL.UpdateChannel.STABLE)
      Log.i(TAG, "yt-dlp update: $status")
      prefs.edit().putLong("ytDlpCheckedAt", now).apply()
    } catch (e: Throwable) {
      Log.w(TAG, "yt-dlp update failed", e)
    }
  }
}
