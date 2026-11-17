// SPDX-License-Identifier: GPL-3.0-or-later
package com.coderzonebd.whoosh

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.os.Build
import android.os.IBinder

/**
 * Keeps the app's process — and with it the download engine running inside it — alive while
 * something downloads, with a notification that says so (Android stops a background app's
 * process soon after its screen is left). The engine starts and stops it through [WdmPlugin]
 * as downloads begin and end; it never decides anything by itself.
 */
class DownloadService : Service() {
  override fun onBind(intent: Intent?): IBinder? = null

  override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
    val title = intent?.getStringExtra(EXTRA_TITLE) ?: "Whoosh Download Manager"
    val text = intent?.getStringExtra(EXTRA_TEXT) ?: ""
    val notification = build(this, title, text)
    if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
      startForeground(NOTIFICATION_ID, notification, ServiceInfo.FOREGROUND_SERVICE_TYPE_DATA_SYNC)
    } else {
      startForeground(NOTIFICATION_ID, notification)
    }
    // Restarted by the system after being killed: nothing to keep going then (the engine
    // resumes its downloads when the app opens again).
    return START_NOT_STICKY
  }

  override fun onTimeout(startId: Int, fgsType: Int) {
    // Android 15 limits dataSync services to six hours a day: stop cleanly when told to.
    stopSelf()
  }

  companion object {
    const val NOTIFICATION_ID = 4210
    const val CHANNEL_ID = "downloads"
    const val EXTRA_TITLE = "title"
    const val EXTRA_TEXT = "text"

    fun build(context: Context, title: String, text: String): Notification {
      val manager = context.getSystemService(NotificationManager::class.java)
      if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O && manager.getNotificationChannel(CHANNEL_ID) == null) {
        manager.createNotificationChannel(
          NotificationChannel(CHANNEL_ID, "Downloads", NotificationManager.IMPORTANCE_LOW).apply {
            description = "Shown while Whoosh Download Manager downloads in the background"
            setShowBadge(false)
          }
        )
      }
      val open = PendingIntent.getActivity(
        context,
        0,
        Intent(context, MainActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP),
        PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
      )
      val builder = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
        Notification.Builder(context, CHANNEL_ID)
      } else {
        @Suppress("DEPRECATION") Notification.Builder(context)
      }
      return builder
        .setSmallIcon(android.R.drawable.stat_sys_download)
        .setContentTitle(title)
        .setContentText(text)
        .setContentIntent(open)
        .setOngoing(true)
        .setOnlyAlertOnce(true)
        .setShowWhen(false)
        .build()
    }
  }
}
