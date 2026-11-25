package com.pollisnative.share

import android.app.Activity
import android.content.Intent
import android.graphics.Color
import android.graphics.drawable.GradientDrawable
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.view.Gravity
import android.widget.FrameLayout
import android.widget.ProgressBar
import java.util.concurrent.Executors

/**
 * Where another app's share sheet sends a share to Pollis (#2100). It copies what was shared
 * ([ShareInbox.intake]) while it still holds the read grant, brings the app forward with a plain
 * launch intent, and finishes. The app takes the share after the PIN and opens its picker.
 *
 * Why not MainActivity, for the reason PushTapActivity exists (#1614): when Android has reclaimed
 * the backgrounded app but kept its task, it re-creates MainActivity from the task's original
 * intent and hands a new one to `onNewIntent` while expo-updates is still choosing the bundle,
 * and expo drops it. Nothing about the share travels in the launch intent here; the app reads it
 * from [ShareInbox].
 *
 * Disabled in the manifest and enabled only while someone is signed in
 * (`PollisShareModule.setShareTargetEnabled`), so a signed-out Pollis is not in the share sheet.
 * It publishes no share targets and no shortcuts: the share sheet shows the app, never a chat.
 */
class ShareIntakeActivity : Activity() {
  private val main = Handler(Looper.getMainLooper())

  override fun onCreate(savedInstanceState: Bundle?) {
    super.onCreate(savedInstanceState)
    // Re-created from saved state: the share was taken in the first run.
    if (savedInstanceState != null) {
      openApp()
      return
    }
    val shared = intent
    if (shared == null || (shared.action != Intent.ACTION_SEND && shared.action != Intent.ACTION_SEND_MULTIPLE)) {
      finish()
      return
    }
    // A large file takes a moment to copy: show that something is happening, but only if it
    // does, so a quick share never flashes a spinner.
    val spinner = Runnable { showSpinner() }
    main.postDelayed(spinner, SPINNER_DELAY_MS)
    worker.execute {
      ShareInbox.intake(applicationContext, shared)
      main.post {
        main.removeCallbacks(spinner)
        openApp()
      }
    }
  }

  private fun showSpinner() {
    if (isFinishing) {
      return
    }
    val density = resources.displayMetrics.density
    val card = FrameLayout(this).apply {
      background = GradientDrawable().apply {
        setColor(Color.parseColor("#E6141210"))
        cornerRadius = 16 * density
      }
      val pad = (24 * density).toInt()
      setPadding(pad, pad, pad, pad)
      addView(ProgressBar(this@ShareIntakeActivity))
    }
    val root = FrameLayout(this)
    root.addView(
      card,
      FrameLayout.LayoutParams(
        FrameLayout.LayoutParams.WRAP_CONTENT,
        FrameLayout.LayoutParams.WRAP_CONTENT,
        Gravity.CENTER,
      ),
    )
    setContentView(root)
  }

  private fun openApp() {
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

  companion object {
    private const val SPINNER_DELAY_MS = 300L
    private val worker = Executors.newSingleThreadExecutor()
  }
}
