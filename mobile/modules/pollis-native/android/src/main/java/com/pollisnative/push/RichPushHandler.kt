package com.pollisnative.push

import android.content.Context
import android.os.SystemClock
import android.util.Log
import com.pollisnative.PollisNotify
import java.util.concurrent.ExecutorService
import java.util.concurrent.Executors
import java.util.concurrent.TimeUnit
import java.util.concurrent.TimeoutException

/**
 * One rich push, end to end (#1599): headless fetch with a deadline, then post. Runs on the
 * caller's thread (FCM's service worker thread, or a receiver's goAsync thread), never the main one.
 *
 * Timing: FCM gives a high-priority data message about 10 s of `onMessageReceived` before the
 * process may be cut back, so the whole handler finishes inside [HANDLER_BUDGET_MS]. Fetches run one
 * at a time on [worker] (the core's state lock would refuse a second anyway); if one overruns, the
 * generic text goes up at the deadline and a late result replaces it in place (same tag, alert once).
 */
object RichPushHandler {
  private const val TAG = "PollisRichPush"
  const val HANDLER_BUDGET_MS = 8_500L
  /** Headroom between the core's own budget and the hard deadline: JNI, JSON, posting. */
  private const val MARGIN_MS = 700L

  private val worker: ExecutorService = Executors.newSingleThreadExecutor { r ->
    Thread(r, "pollis-rich-push").apply { isDaemon = true }
  }

  /** The directory the app's core runs in: Expo's `FileSystem.documentDirectory`, as `lib/native/bridge.ts` passes it. */
  fun dataDir(context: Context): String = context.filesDir.absolutePath + "/"

  /**
   * @param fetch the core call; tests and the debug hook substitute it.
   */
  fun handle(
    context: Context,
    handle: String,
    data: Map<String, String>,
    messageId: String?,
    sentTime: Long,
    fetch: (dataDir: String, handle: String, budgetMs: Int) -> String = { dir, h, budget -> PollisNotify.fetch(dir, h, budget) },
  ) {
    val app = context.applicationContext
    val notifier = RichPushNotifier(app)
    val start = SystemClock.elapsedRealtime()
    val deadline = start + HANDLER_BUDGET_MS
    val dir = dataDir(app)
    val future = worker.submit<String> {
      // Time spent queued behind an earlier push comes out of this one's budget.
      val budget = (deadline - MARGIN_MS - SystemClock.elapsedRealtime()).coerceAtLeast(0L)
      if (budget == 0L) {
        """{"ok":false,"reason":"timeout"}"""
      } else {
        fetch(dir, handle, budget.toInt())
      }
    }
    val wait = (deadline - SystemClock.elapsedRealtime()).coerceAtLeast(0L)
    val json = try {
      future.get(wait, TimeUnit.MILLISECONDS)
    } catch (e: TimeoutException) {
      null
    } catch (e: Exception) {
      """{"ok":false,"reason":"internal"}"""
    }
    if (json != null) {
      post(notifier, handle, json, data, messageId, sentTime)
      return
    }
    // Overran: never leave the push unshown. A late answer upgrades it if the process lives.
    Log.i(TAG, "fetch overran its budget; posting the generic notification")
    notifier.post(handle, RichPush.Plan.Generic, data, messageId, sentTime)
    worker.execute {
      val late = try {
        future.get()
      } catch (e: Exception) {
        null
      }
      if (late != null) {
        post(notifier, handle, late, data, messageId, sentTime)
      }
    }
  }

  private fun post(notifier: RichPushNotifier, handle: String, json: String, data: Map<String, String>, messageId: String?, sentTime: Long) {
    val plan = RichPush.plan(json)
    // Only the outcome class, never content.
    Log.i(TAG, "posting ${plan.javaClass.simpleName}")
    try {
      notifier.post(handle, plan, data, messageId, sentTime)
    } catch (e: Exception) {
      Log.w(TAG, "rich post failed; posting the generic notification", e)
      try {
        notifier.post(handle, RichPush.Plan.Generic, data, messageId, sentTime)
      } catch (e2: Exception) {
        Log.w(TAG, "generic post failed", e2)
      }
    }
  }
}
