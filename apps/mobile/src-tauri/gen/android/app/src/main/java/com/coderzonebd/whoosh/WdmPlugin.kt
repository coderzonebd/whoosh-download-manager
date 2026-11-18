// SPDX-License-Identifier: GPL-3.0-or-later
package com.coderzonebd.whoosh

import android.app.Activity
import android.app.NotificationManager
import android.content.Intent
import android.os.Build
import app.tauri.annotation.Command
import app.tauri.annotation.InvokeArg
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Invoke
import app.tauri.plugin.Plugin
import kotlin.concurrent.thread

@InvokeArg
class ActiveArgs {
  var active: Int = 0
  var title: String = ""
  var text: String = ""
}

/**
 * The engine's (Rust) way to the phone: `setActive` keeps [DownloadService] running while
 * something downloads and updates its notification, and stops it when nothing does.
 */
@TauriPlugin
class WdmPlugin(private val activity: Activity) : Plugin(activity) {
  private var running = false

  /** yt-dlp, ffmpeg and QuickJS for the engine ([MediaTools]); waits for the first unpack. */
  @Command
  fun mediaTools(invoke: Invoke) {
    android.util.Log.i("WdmMediaTools", "mediaTools asked")
    MediaTools.start(activity)
    thread(name = "wdm-media-tools-wait", isDaemon = true) {
      val tools = MediaTools.await(120_000)
      if (tools != null) {
        invoke.resolve(tools)
      } else {
        invoke.reject(MediaTools.error ?: "the media tools are still unpacking")
      }
    }
  }

  @Command
  fun setActive(invoke: Invoke) {
    val args = invoke.parseArgs(ActiveArgs::class.java)
    val context = activity.applicationContext
    if (args.active <= 0) {
      if (running) {
        context.stopService(Intent(context, DownloadService::class.java))
        running = false
      }
      invoke.resolve()
      return
    }
    if (!running) {
      val start = Intent(context, DownloadService::class.java)
        .putExtra(DownloadService.EXTRA_TITLE, args.title)
        .putExtra(DownloadService.EXTRA_TEXT, args.text)
      try {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
          context.startForegroundService(start)
        } else {
          context.startService(start)
        }
        running = true
      } catch (e: Exception) {
        // Not allowed from the background (Android 12+): the next update, with the app in
        // front, starts it.
        invoke.reject("cannot start the download service: ${e.message}")
        return
      }
    } else {
      context.getSystemService(NotificationManager::class.java)
        .notify(DownloadService.NOTIFICATION_ID, DownloadService.build(context, args.title, args.text))
    }
    invoke.resolve()
  }
}
