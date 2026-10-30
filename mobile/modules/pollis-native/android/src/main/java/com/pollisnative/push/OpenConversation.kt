package com.pollisnative.push

import android.app.ActivityManager
import com.facebook.react.bridge.ReactApplicationContext
import com.facebook.react.bridge.ReactContextBaseJavaModule
import com.facebook.react.bridge.ReactMethod

/**
 * The conversation on screen right now, as JS reports it (#1599): set when a chat gains focus,
 * cleared on blur and when the app leaves the foreground. In memory only, never persisted; a cold
 * process (no JS) has none, which is right, since then nothing is on screen.
 */
object OpenConversation {
  @Volatile
  var id: String? = null

  fun isAppInForeground(): Boolean {
    val info = ActivityManager.RunningAppProcessInfo()
    ActivityManager.getMyMemoryState(info)
    return info.importance == ActivityManager.RunningAppProcessInfo.IMPORTANCE_FOREGROUND
  }
}

/** JS side: `NativeModules.PollisPushState.setOpenConversation(id | null)` (`lib/push/openConversation.ts`). */
class PollisPushStateModule(context: ReactApplicationContext) : ReactContextBaseJavaModule(context) {
  override fun getName(): String = NAME

  @ReactMethod
  fun setOpenConversation(id: String?) {
    OpenConversation.id = id?.takeIf { it.isNotEmpty() }
  }

  /** The live theme bases ("#rrggbb"), kept for the letter avatars a cold process draws. */
  @ReactMethod
  fun setThemeColors(accentHex: String, bgHex: String) {
    LetterAvatar.saveTheme(reactApplicationContext, accentHex, bgHex)
  }

  companion object {
    const val NAME = "PollisPushState"
  }
}
