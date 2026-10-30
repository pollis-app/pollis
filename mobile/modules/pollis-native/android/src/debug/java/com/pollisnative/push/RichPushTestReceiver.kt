package com.pollisnative.push

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.util.Log

/**
 * Debug-only stand-in for an FCM delivery (#1599): FCM's own receiver only accepts Google Play
 * services, so this feeds the same [RichPush.richHandle] + [RichPushHandler] path from adb.
 *
 *   adb shell am broadcast -a com.pollis.mobile.debug.RICH_PUSH -p com.pollis.mobile \
 *     --es body '{"h":"AAAAAAAAAAAAAAAAAAAAAA"}' [--es fake '<fetch JSON>']
 *
 * Every string extra except `fake` becomes FCM data. `fake` replaces the core call with a canned
 * reply, to exercise each rendering path without an account.
 */
class RichPushTestReceiver : BroadcastReceiver() {
  override fun onReceive(context: Context, intent: Intent) {
    val extras = intent.extras ?: return
    val data = HashMap<String, String>()
    for (key in extras.keySet()) {
      if (key == "fake") {
        continue
      }
      extras.getString(key)?.let { data[key] = it }
    }
    val fake = extras.getString("fake")
    val handle = RichPush.richHandle(data, false)
    if (handle == null) {
      Log.i("PollisRichPush", "test push is not a rich push; ignored")
      return
    }
    val pending = goAsync()
    Thread {
      try {
        if (fake != null) {
          RichPushHandler.handle(context, handle, data, null, System.currentTimeMillis()) { _, _, _ -> fake }
        } else {
          RichPushHandler.handle(context, handle, data, null, System.currentTimeMillis())
        }
      } finally {
        pending.finish()
      }
    }.start()
  }
}
