package com.pollisnative.share

import android.content.ComponentName
import android.content.pm.PackageManager
import com.facebook.react.bridge.Promise
import com.facebook.react.bridge.ReactApplicationContext
import com.facebook.react.bridge.ReactContextBaseJavaModule
import com.facebook.react.bridge.ReactMethod
import java.util.concurrent.Executors

/**
 * The app's side of sharing into Pollis (#2100). JS: `NativeModules.PollisShare`
 * (`lib/share/native.ts`).
 */
class PollisShareModule(context: ReactApplicationContext) : ReactContextBaseJavaModule(context) {
  override fun getName(): String = NAME

  /** The pending share's manifest JSON, or null; it is no longer pending afterwards. */
  @ReactMethod
  fun takePendingShare(promise: Promise) {
    worker.execute {
      try {
        promise.resolve(ShareInbox.take(reactApplicationContext))
      } catch (e: Throwable) {
        promise.reject("E_SHARE", e.message ?: "no share", e)
      }
    }
  }

  /** Delete one share's copied files: sent, or cancelled. */
  @ReactMethod
  fun discardShare(id: String, promise: Promise) {
    worker.execute {
      ShareInbox.discard(reactApplicationContext, id)
      promise.resolve(null)
    }
  }

  /** Delete every share's files (sign-out). */
  @ReactMethod
  fun discardAllShares(promise: Promise) {
    worker.execute {
      ShareInbox.discardAll(reactApplicationContext)
      promise.resolve(null)
    }
  }

  /**
   * Whether Pollis is offered in other apps' share sheets: only while someone is signed in, so a
   * signed-out install never takes a share it could not send.
   */
  @ReactMethod
  fun setShareTargetEnabled(enabled: Boolean, promise: Promise) {
    try {
      val component = ComponentName(reactApplicationContext, ShareIntakeActivity::class.java)
      val state = if (enabled) {
        PackageManager.COMPONENT_ENABLED_STATE_ENABLED
      } else {
        PackageManager.COMPONENT_ENABLED_STATE_DISABLED
      }
      val pm = reactApplicationContext.packageManager
      if (pm.getComponentEnabledSetting(component) != state) {
        pm.setComponentEnabledSetting(component, state, PackageManager.DONT_KILL_APP)
      }
      promise.resolve(null)
    } catch (e: Throwable) {
      promise.reject("E_SHARE", e.message ?: "share target not changed", e)
    }
  }

  companion object {
    const val NAME = "PollisShare"
    private val worker = Executors.newSingleThreadExecutor()
  }
}
