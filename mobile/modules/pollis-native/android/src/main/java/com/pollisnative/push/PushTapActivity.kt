package com.pollisnative.push

import android.app.Activity
import android.content.Intent
import android.os.Bundle
import android.util.Log
import expo.modules.notifications.notifications.NotificationManager

/**
 * Where the tap on every notification [RichPushNotifier] posts lands (#1614): it hands the tap to
 * expo-notifications itself, then brings the app forward and finishes. Never shown.
 *
 * Why not open MainActivity with the push as extras, as FCM's own notification does: expo reads a
 * tap from the launch intent only in MainActivity's `onCreate`, or in `onNewIntent` once the app
 * has loaded. When the process is gone but the task is still in Recents (Android reclaimed the
 * backgrounded app, the common case), Android re-creates MainActivity from the task's ORIGINAL
 * intent and passes the tap to `onNewIntent` straight after, while expo-updates is still deciding
 * which bundle to launch; expo's `ReactActivityDelegateWrapper` drops that `onNewIntent`, so the
 * app opened on Groups instead of the conversation.
 *
 * [NotificationManager.onNotificationResponseFromExtras] is the call expo's own lifecycle listener
 * makes with those extras: a running app gets the response event at once, and a cold one gets it
 * as the launch response (`getLastNotificationResponse`) when its JS starts. MainActivity is then
 * started with a plain launch intent, so expo never sees the tap a second time.
 */
class PushTapActivity : Activity() {
  override fun onCreate(savedInstanceState: Bundle?) {
    super.onCreate(savedInstanceState)
    val extras = intent?.extras
    // Recreated from saved state (process death with this on top), the tap was already handed over.
    if (savedInstanceState == null && extras != null && extras.containsKey(RichPush.GOOGLE_MESSAGE_ID)) {
      try {
        NotificationManager.onNotificationResponseFromExtras(Bundle(extras))
      } catch (e: Exception) {
        Log.w("PollisRichPush", "tap not handed to expo-notifications", e)
      }
    }
    val launch = packageManager.getLaunchIntentForPackage(packageName)
    if (launch != null) {
      // What the launcher does: bring the app's task forward as it was, or start it.
      launch.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_RESET_TASK_IF_NEEDED)
      launch.setPackage(packageName)
      startActivity(launch)
    }
    finish()
    overridePendingTransition(0, 0)
  }
}
