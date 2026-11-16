package com.pollisnative.push

import android.app.Notification
import android.app.NotificationManager
import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.os.Build
import android.os.IBinder
import android.util.Log
import androidx.core.app.NotificationCompat
import androidx.core.content.ContextCompat
import com.pollisnative.R

/**
 * Network for a message push's fetch, whatever the phone restricts (#1804).
 *
 * Android's Data Saver blocks a backgrounded app's network on a metered connection, and a
 * high-priority FCM message lifts Doze and standby limits but not that one, so the fetch could
 * never reach the Delivery Service and every notification on mobile data fell back to "New
 * message". A foreground service lifts it: a process running one counts as foreground for
 * network access. Android 12+ allows starting one from a high-priority FCM message, which is
 * exactly when this runs. The same approach Signal takes.
 *
 * The service does no work itself; [RichPushHandler] fetches on its own thread and holds the
 * service around it through [PushFetchKeeper]. Android shows a foreground service's notification
 * only after about ten seconds (12+), so a normal fetch is over before anything appears.
 */
class PushFetchService : Service(), PushFetchKeeper.Foreground {
  /**
   * The newest start delivered to this instance. [leave] stops only up to it: a plain `stopSelf()`
   * racing a new push's `startForegroundService` would destroy the service before that start's
   * `startForeground`, and Android ends the app for it.
   */
  @Volatile private var lastStartId = 0

  override fun onBind(intent: Intent?): IBinder? = null

  override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
    lastStartId = startId
    PushFetchKeeper.onStarted(this)
    return START_NOT_STICKY
  }

  override fun onDestroy() {
    PushFetchKeeper.onDestroyed(this)
    super.onDestroy()
  }

  /** Must run within five seconds of `startForegroundService`, or Android ends the app. */
  override fun enter(): Boolean {
    return try {
      val notification = notification(this)
      if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.UPSIDE_DOWN_CAKE) {
        startForeground(NOTIFICATION_ID, notification, ServiceInfo.FOREGROUND_SERVICE_TYPE_REMOTE_MESSAGING)
      } else {
        startForeground(NOTIFICATION_ID, notification)
      }
      true
    } catch (e: Exception) {
      Log.w(TAG, "could not enter the foreground", e)
      false
    }
  }

  override fun leave() {
    try {
      if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.N) {
        stopForeground(STOP_FOREGROUND_REMOVE)
      } else {
        @Suppress("DEPRECATION")
        stopForeground(true)
      }
    } catch (e: Exception) {
      Log.w(TAG, "stopForeground failed", e)
    }
    // False when a newer start is already on its way: its onStartCommand enters the foreground again.
    stopSelfResult(lastStartId)
  }

  companion object {
    private const val TAG = "PollisPushFetch"
    const val CHANNEL_ID = "pollis_push_fetch"
    /** Distinct from the message notifications' id 0 (tagged by handle). */
    const val NOTIFICATION_ID = 1804

    private fun notification(context: Context): Notification {
      ensureChannel(context)
      return NotificationCompat.Builder(context, CHANNEL_ID)
        .setSmallIcon(context.applicationInfo.icon)
        .setContentText(context.getString(R.string.pollis_push_fetching_body))
        .setPriority(NotificationCompat.PRIORITY_MIN)
        .setCategory(NotificationCompat.CATEGORY_SERVICE)
        .setOngoing(true)
        .setSilent(true)
        .setShowWhen(false)
        .build()
    }

    /** Its own quiet channel, so the user can hide it without touching message notifications. */
    private fun ensureChannel(context: Context) {
      ensureNotificationChannel(context, CHANNEL_ID, R.string.pollis_push_fetching_channel, NotificationManager.IMPORTANCE_MIN)
    }

    internal fun start(context: Context): Boolean {
      return try {
        ContextCompat.startForegroundService(context, Intent(context, PushFetchService::class.java))
        true
      } catch (e: Exception) {
        // ForegroundServiceStartNotAllowedException (12+) when the push was not high priority
        // after all, e.g. FCM downgraded it. The fetch still runs, as before #1804.
        Log.w(TAG, "foreground service not allowed", e)
        false
      }
    }
  }
}

/**
 * Who needs [PushFetchService] right now. Pushes overlap, so it is refcounted: the first
 * [acquire] starts it and the last [release] stops it.
 *
 * Android ends the app if a service started with `startForegroundService` stops before it calls
 * `startForeground`, so a release that lands before the service has started never stops it from
 * outside: the service enters the foreground on start, then stops itself if nothing holds it.
 */
object PushFetchKeeper {
  /** The running service, as the keeper sees it (a fake in tests). */
  interface Foreground {
    /** Call `startForeground`; false if Android refused. */
    fun enter(): Boolean
    /** Leave the foreground and stop. */
    fun leave()
  }

  private val lock = Any()
  private var holds = 0
  /** A start was asked for and the service has not reported in yet. */
  private var starting = false
  private var running: Foreground? = null

  /** Hold the service for one fetch. Returns false when Android refused to start it. */
  fun acquire(context: Context): Boolean = acquire { PushFetchService.start(context.applicationContext) }

  internal fun acquire(start: () -> Boolean): Boolean {
    synchronized(lock) {
      holds += 1
      if (running != null || starting) {
        return true
      }
      starting = true
    }
    val started = start()
    if (!started) {
      synchronized(lock) {
        starting = false
        holds = (holds - 1).coerceAtLeast(0)
      }
    }
    return started
  }

  /** Give back one [acquire] that returned true. */
  fun release() {
    val stop: Foreground?
    synchronized(lock) {
      holds = (holds - 1).coerceAtLeast(0)
      stop = if (holds == 0) running.also { running = null } else null
    }
    stop?.leave()
  }

  internal fun onStarted(service: Foreground) {
    service.enter()
    val stop: Boolean
    synchronized(lock) {
      starting = false
      if (holds == 0) {
        stop = true
      } else {
        running = service
        stop = false
      }
    }
    if (stop) {
      service.leave()
    }
  }

  internal fun onDestroyed(service: Foreground) {
    synchronized(lock) {
      if (running === service) {
        running = null
      }
    }
  }

  /** Tests only. */
  internal fun holdsForTest(): Int = synchronized(lock) { holds }

  /** Tests only. */
  internal fun resetForTest() {
    synchronized(lock) {
      holds = 0
      starting = false
      running = null
    }
  }
}
