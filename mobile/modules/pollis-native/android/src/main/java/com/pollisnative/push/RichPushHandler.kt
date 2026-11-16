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
 *
 * Network (#1804): unless the app is on screen, the handler holds [PushFetchService] from before
 * the fetch until the core's deferred posts (watermark, delivered receipt) have settled, so Data
 * Saver cannot block the fetch and the process lives long enough for a late result.
 */
object RichPushHandler {
  private const val TAG = "PollisRichPush"
  const val HANDLER_BUDGET_MS = 8_500L
  /** Headroom between the core's own budget and the hard deadline: JNI, JSON, posting. */
  private const val MARGIN_MS = 700L
  /** Longest the handler waits, after posting, for the core's deferred posts (#1804). */
  const val SETTLE_MS = 20_000
  /** Longest an overrun fetch may still upgrade its notification. */
  private const val LATE_MS = 60_000L

  private val worker: ExecutorService = Executors.newSingleThreadExecutor { r ->
    Thread(r, "pollis-rich-push").apply { isDaemon = true }
  }

  /** Settling and releasing the service, off [worker] so the next push's fetch never queues behind it. */
  private val tail: ExecutorService = Executors.newSingleThreadExecutor { r ->
    Thread(r, "pollis-rich-push-tail").apply { isDaemon = true }
  }

  /** The directory the app's core runs in: Expo's `FileSystem.documentDirectory`, as `lib/native/bridge.ts` passes it. */
  fun dataDir(context: Context): String = context.filesDir.absolutePath + "/"

  /**
   * @param fetch the core call; tests and the debug hook substitute it.
   * @param hold take [PushFetchService] for this push; false when it was not taken.
   */
  fun handle(
    context: Context,
    handle: String,
    data: Map<String, String>,
    messageId: String?,
    sentTime: Long,
    fetch: (dataDir: String, handle: String, budgetMs: Int) -> String = { dir, h, budget -> PollisNotify.fetch(dir, h, budget) },
    note: (line: String) -> Unit = { PollisNotify.note(it) },
    hold: (Context) -> Boolean = { c -> !OpenConversation.isAppInForeground() && PushFetchKeeper.acquire(c) },
  ) {
    val app = context.applicationContext
    // Before the fetch, so the network is open by the time it asks (#1804).
    val held = try {
      hold(app)
    } catch (e: Exception) {
      false
    }
    val done = { finish(held) }
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
    if (json == null || RichPush.outcomeDetail(json).let { it == "reason=network" || it == "reason=timeout" }) {
      // Why the network failed is Android's to say (#1672): record its restrictions as they are now.
      note("android " + PushNetworkState.describe(app))
    }
    if (json != null) {
      try {
        post(notifier, handle, json, data, messageId, sentTime)
      } finally {
        done()
      }
      return
    }
    // Overran: never leave the push unshown. A late answer upgrades it if the process lives.
    Log.i(TAG, "fetch overran its budget; posting the generic notification")
    try {
      notifier.post(handle, RichPush.Plan.Generic, data, messageId, sentTime)
    } catch (e: Exception) {
      Log.w(TAG, "generic post failed", e)
    }
    tail.execute {
      try {
        val late = try {
          future.get(LATE_MS, TimeUnit.MILLISECONDS)
        } catch (e: Exception) {
          null
        }
        if (late != null) {
          post(notifier, handle, late, data, messageId, sentTime)
        }
      } finally {
        done()
      }
    }
  }

  /**
   * After the notification is up: let the core's deferred posts finish, then give the service
   * back. On [tail], so neither the FCM thread nor the next fetch waits for it.
   */
  private fun finish(held: Boolean) {
    if (!held) {
      return
    }
    tail.execute {
      try {
        PollisNotify.settle(SETTLE_MS)
      } finally {
        PushFetchKeeper.release()
      }
    }
  }

  private fun post(notifier: RichPushNotifier, handle: String, json: String, data: Map<String, String>, messageId: String?, sentTime: Long) {
    val plan = RichPush.plan(json)
    if (RichPush.suppressForOpenConversation(plan, OpenConversation.isAppInForeground(), OpenConversation.id)) {
      // The chat is on screen: show nothing (and drop a generic one posted at the deadline).
      Log.i(TAG, "suppressed: conversation on screen")
      notifier.post(handle, RichPush.Plan.Silent, data, messageId, sentTime)
      return
    }
    // Only the outcome class and the core's reason or mode, never content.
    Log.i(TAG, "posting ${plan.outcome} (${RichPush.outcomeDetail(json)})")
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
