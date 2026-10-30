package com.pollisnative.push

import android.app.ActivityManager
import android.util.Log
import com.pollisnative.BuildConfig
import com.google.firebase.messaging.RemoteMessage
import expo.modules.notifications.service.ExpoFirebaseMessagingService

/**
 * The app's one FCM service (#1599). It IS expo-notifications' service (subclass), declared in its
 * place: `plugins/withRichPush.js` removes expo's own `<service>` from the merged manifest, so FCM
 * has exactly one MESSAGING_EVENT target and nothing depends on intent-filter priority.
 *
 * A rich push (data-only, carrying `h`, no title/body) is handled natively: fetched and decrypted on
 * device, posted as MessagingStyle. Everything else (today's alert pushes, #1508 `{d}` dismissals,
 * token rotation, deleted-messages) goes to expo unchanged via `super`.
 */
class PollisMessagingService : ExpoFirebaseMessagingService() {
  override fun onMessageReceived(remoteMessage: RemoteMessage) {
    val data = remoteMessage.data
    if (BuildConfig.DEBUG) {
      // Keys only, never values: how the Expo push service shaped this delivery.
      Log.d("PollisRichPush", "FCM data keys=${data.keys.sorted()} notification=${remoteMessage.notification != null}")
    }
    val handle = RichPush.richHandle(data, remoteMessage.notification != null)
    if (handle == null) {
      super.onMessageReceived(remoteMessage)
      return
    }
    RichPushHandler.handle(this, handle, data, remoteMessage.messageId, remoteMessage.sentTime)
    if (isForeground()) {
      // Let JS see it, as it sees every push in the foreground (`addNotificationReceivedListener`
      // ingests that conversation). Its handler shows nothing for a title-less push, so this
      // never posts a second, blank notification. In the background expo would only start a
      // headless JS task here for nothing, so it is skipped.
      super.onMessageReceived(remoteMessage)
    }
  }

  private fun isForeground(): Boolean {
    val info = ActivityManager.RunningAppProcessInfo()
    ActivityManager.getMyMemoryState(info)
    return info.importance == ActivityManager.RunningAppProcessInfo.IMPORTANCE_FOREGROUND
  }
}
