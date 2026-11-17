// SPDX-License-Identifier: GPL-3.0-or-later
package com.coderzonebd.whoosh

import android.app.Activity
import android.app.PendingIntent
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.pm.PackageInstaller
import android.net.Uri
import android.os.Build
import android.provider.Settings
import android.util.Log
import java.io.File
import java.net.HttpURLConnection
import java.net.URL
import java.security.MessageDigest
import kotlin.concurrent.thread

/**
 * Updates the app from a GitHub release without the browser: downloads the release's APK, checks
 * it against the SHA-256 GitHub lists for it, and hands it to Android's package installer. Android
 * asks the person to confirm (an app from outside a store cannot replace itself silently the
 * first time); on Android 12+ later updates may go through without asking, once this app is the
 * one that installed itself. Android refuses an APK signed with another key.
 *
 * Progress goes to [listener]: "downloading", "installing", "permission" (the person must allow
 * installing apps from WDM first; Settings is opened), or "failed: <reason>".
 */
object AppUpdate {
  private const val TAG = "WdmAppUpdate"
  private const val RELEASES = "https://github.com/coderzonebd/whoosh-download-manager/releases/download/"
  private const val MAX_BYTES = 300L shl 20
  const val ACTION_STATUS = "com.coderzonebd.whoosh.UPDATE_STATUS"

  @Volatile var listener: ((String) -> Unit)? = null
  @Volatile private var busy = false

  private fun tell(state: String) {
    Log.i(TAG, state)
    listener?.invoke(state)
  }

  fun start(activity: Activity, url: String, sha256: String) {
    if (!url.startsWith(RELEASES) || !url.endsWith(".apk") || !sha256.matches(Regex("^[0-9a-fA-F]{64}$"))) {
      tell("failed: not a release of this app")
      return
    }
    if (!activity.packageManager.canRequestPackageInstalls()) {
      tell("permission")
      activity.runOnUiThread {
        try {
          activity.startActivity(
            Intent(Settings.ACTION_MANAGE_UNKNOWN_APP_SOURCES, Uri.parse("package:${activity.packageName}")),
          )
        } catch (_: Exception) {
        }
      }
      return
    }
    synchronized(this) {
      if (busy) return
      busy = true
    }
    val app = activity.applicationContext
    thread(name = "wdm-app-update", isDaemon = true) {
      try {
        tell("downloading")
        val apk = download(app, url, sha256.lowercase())
        tell("installing")
        install(app, apk)
      } catch (e: Exception) {
        Log.w(TAG, "update failed", e)
        tell("failed: ${e.message ?: e}")
      } finally {
        busy = false
      }
    }
  }

  private fun download(app: Context, url: String, sha256: String): File {
    val dir = File(app.cacheDir, "update").apply { mkdirs() }
    dir.listFiles()?.forEach { it.delete() }
    val file = File(dir, "update.apk")
    var conn = URL(url).openConnection() as HttpURLConnection
    // GitHub answers with a redirect to its download host.
    conn.instanceFollowRedirects = true
    conn.connectTimeout = 20_000
    conn.readTimeout = 60_000
    conn.connect()
    if (conn.responseCode != 200) throw IllegalStateException("HTTP ${conn.responseCode}")
    val digest = MessageDigest.getInstance("SHA-256")
    var total = 0L
    conn.inputStream.use { input ->
      file.outputStream().use { out ->
        val buf = ByteArray(64 * 1024)
        while (true) {
          val n = input.read(buf)
          if (n < 0) break
          total += n
          if (total > MAX_BYTES) throw IllegalStateException("the download is too large")
          digest.update(buf, 0, n)
          out.write(buf, 0, n)
        }
      }
    }
    conn.disconnect()
    val got = digest.digest().joinToString("") { "%02x".format(it) }
    if (got != sha256) {
      file.delete()
      throw IllegalStateException("the download does not match its checksum")
    }
    return file
  }

  private fun install(app: Context, apk: File) {
    val installer = app.packageManager.packageInstaller
    val params = PackageInstaller.SessionParams(PackageInstaller.SessionParams.MODE_FULL_INSTALL)
    params.setAppPackageName(app.packageName)
    if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
      // Allowed without asking only when this app installed itself before; else Android asks.
      params.setRequireUserAction(PackageInstaller.SessionParams.USER_ACTION_NOT_REQUIRED)
    }
    val id = installer.createSession(params)
    installer.openSession(id).use { session ->
      apk.inputStream().use { input ->
        session.openWrite("base.apk", 0, apk.length()).use { out ->
          input.copyTo(out)
          session.fsync(out)
        }
      }
      val status = Intent(app, UpdateStatusReceiver::class.java).setAction(ACTION_STATUS)
      val flags = PendingIntent.FLAG_UPDATE_CURRENT or
        (if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) PendingIntent.FLAG_MUTABLE else 0)
      session.commit(PendingIntent.getBroadcast(app, id, status, flags).intentSender)
    }
  }

  internal fun onStatus(context: Context, intent: Intent) {
    when (val code = intent.getIntExtra(PackageInstaller.EXTRA_STATUS, PackageInstaller.STATUS_FAILURE)) {
      PackageInstaller.STATUS_PENDING_USER_ACTION -> {
        @Suppress("DEPRECATION")
        val confirm = intent.getParcelableExtra<Intent>(Intent.EXTRA_INTENT) ?: return
        context.startActivity(confirm.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK))
      }
      PackageInstaller.STATUS_SUCCESS -> tell("installed")
      else -> {
        val msg = intent.getStringExtra(PackageInstaller.EXTRA_STATUS_MESSAGE) ?: "status $code"
        tell(if (code == PackageInstaller.STATUS_FAILURE_ABORTED) "cancelled" else "failed: $msg")
      }
    }
  }
}

/** Android's answer to [AppUpdate]'s install session. */
class UpdateStatusReceiver : BroadcastReceiver() {
  override fun onReceive(context: Context, intent: Intent) {
    AppUpdate.onStatus(context, intent)
  }
}
