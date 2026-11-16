package com.pollisnative.push

import android.app.NotificationChannel
import android.app.NotificationManager
import android.content.Context
import android.os.Build

/** Create notification channel [id] (no badge) unless it exists. A no-op before Android 8. */
internal fun ensureNotificationChannel(context: Context, id: String, nameRes: Int, importance: Int) {
  if (Build.VERSION.SDK_INT < Build.VERSION_CODES.O) {
    return
  }
  val system = context.getSystemService(NotificationManager::class.java) ?: return
  if (system.getNotificationChannel(id) != null) {
    return
  }
  val channel = NotificationChannel(id, context.getString(nameRes), importance)
  channel.setShowBadge(false)
  system.createNotificationChannel(channel)
}
