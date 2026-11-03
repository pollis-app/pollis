package com.pollisnative.push

import android.app.ActivityManager
import android.app.usage.UsageStatsManager
import android.content.Context
import android.net.ConnectivityManager
import android.net.NetworkCapabilities
import android.os.Build
import android.os.PowerManager

/**
 * What Android is doing to the app's network, as one diagnostics line, for a push fetch that
 * failed (#1672): the transport and whether it is metered, whether Android reports it blocked
 * for this app, Data Saver, the battery "Restricted" setting, the standby bucket, Doze and the
 * process's importance. The kind of network only, never an address or a network's name.
 */
object PushNetworkState {
  fun describe(context: Context): String {
    val parts = mutableListOf<String>()
    val cm = context.getSystemService(ConnectivityManager::class.java)
    if (cm != null) {
      // activeNetwork is null while Android blocks this app, so fall back to any network that
      // carries the internet: what the phone is on, not what this app may use.
      val active = cm.activeNetwork
      parts += "active=" + (active != null)
      @Suppress("DEPRECATION")
      val caps = (active ?: cm.allNetworks.firstOrNull {
        cm.getNetworkCapabilities(it)?.hasCapability(NetworkCapabilities.NET_CAPABILITY_INTERNET) == true
      })?.let { cm.getNetworkCapabilities(it) }
      parts += "net=" + when {
        caps == null -> "none"
        caps.hasTransport(NetworkCapabilities.TRANSPORT_WIFI) -> "wifi"
        caps.hasTransport(NetworkCapabilities.TRANSPORT_CELLULAR) -> "cellular"
        caps.hasTransport(NetworkCapabilities.TRANSPORT_ETHERNET) -> "ethernet"
        caps.hasTransport(NetworkCapabilities.TRANSPORT_VPN) -> "vpn"
        else -> "other"
      }
      if (caps != null) {
        parts += "metered=" + !caps.hasCapability(NetworkCapabilities.NET_CAPABILITY_NOT_METERED)
        parts += "validated=" + caps.hasCapability(NetworkCapabilities.NET_CAPABILITY_VALIDATED)
      }
      @Suppress("DEPRECATION")
      cm.activeNetworkInfo?.let { parts += "state=" + it.detailedState.name.lowercase() }
      parts += "data_saver=" + when (cm.restrictBackgroundStatus) {
        ConnectivityManager.RESTRICT_BACKGROUND_STATUS_ENABLED -> "on"
        ConnectivityManager.RESTRICT_BACKGROUND_STATUS_WHITELISTED -> "on_exempt"
        else -> "off"
      }
    }
    if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.P) {
      context.getSystemService(ActivityManager::class.java)?.let {
        parts += "battery_restricted=" + it.isBackgroundRestricted
      }
      context.getSystemService(UsageStatsManager::class.java)?.let {
        parts += "bucket=" + bucketName(it.appStandbyBucket)
      }
    }
    context.getSystemService(PowerManager::class.java)?.let {
      parts += "doze=" + it.isDeviceIdleMode
      parts += "battery_optimized=" + !it.isIgnoringBatteryOptimizations(context.packageName)
    }
    val me = ActivityManager.RunningAppProcessInfo()
    ActivityManager.getMyMemoryState(me)
    parts += "importance=" + me.importance
    return parts.joinToString(" ")
  }

  private fun bucketName(bucket: Int): String = when (bucket) {
    UsageStatsManager.STANDBY_BUCKET_ACTIVE -> "active"
    UsageStatsManager.STANDBY_BUCKET_WORKING_SET -> "working_set"
    UsageStatsManager.STANDBY_BUCKET_FREQUENT -> "frequent"
    UsageStatsManager.STANDBY_BUCKET_RARE -> "rare"
    45 -> "restricted" // STANDBY_BUCKET_RESTRICTED, API 30
    else -> bucket.toString()
  }
}
