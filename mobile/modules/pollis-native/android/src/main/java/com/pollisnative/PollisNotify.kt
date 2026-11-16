package com.pollisnative

/**
 * The headless notification fetch (#1599) over JNI: `pollis_notify_fetch` from pollis-core, which
 * is linked into the same libpollis-native.so as the JSI bridge, so a warm process shares the
 * app's core and a cold one (started by FCM, no React) initialises it headless. Never throws.
 */
object PollisNotify {
  private const val FAILED = """{"ok":false,"reason":"internal"}"""

  @Volatile
  private var loaded: Boolean? = null

  private fun ensureLoaded(): Boolean {
    loaded?.let { return it }
    synchronized(this) {
      loaded?.let { return it }
      val ok = try {
        // Idempotent: the JSI module loads the same library in a warm process.
        System.loadLibrary("pollis-native")
        true
      } catch (t: Throwable) {
        false
      }
      loaded = ok
      return ok
    }
  }

  @JvmStatic
  private external fun nativeFetch(dataDir: String, handle: String?, budgetMs: Int): ByteArray?

  @JvmStatic
  private external fun nativeNote(line: String)

  @JvmStatic
  private external fun nativeSettle(timeoutMs: Int): Boolean

  /** Add a line to the core's diagnostics log under `[notify]`; never content. Never throws. */
  fun note(line: String) {
    if (!ensureLoaded()) {
      return
    }
    try {
      nativeNote(line)
    } catch (t: Throwable) {
    }
  }

  /** The core's JSON reply, or a failure reply; blocks for at most about `budgetMs`. */
  fun fetch(dataDir: String, handle: String?, budgetMs: Int): String {
    if (!ensureLoaded()) {
      return FAILED
    }
    return try {
      nativeFetch(dataDir, handle, budgetMs)?.toString(Charsets.UTF_8) ?: FAILED
    } catch (t: Throwable) {
      FAILED
    }
  }

  /**
   * Wait up to [timeoutMs] for the posts earlier fetches left running (#1804): the watermark
   * advance and the delivered receipt, which the notification does not wait for. True when
   * nothing is left. Never throws.
   */
  fun settle(timeoutMs: Int): Boolean {
    if (!ensureLoaded()) {
      return true
    }
    return try {
      nativeSettle(timeoutMs)
    } catch (t: Throwable) {
      true
    }
  }
}
