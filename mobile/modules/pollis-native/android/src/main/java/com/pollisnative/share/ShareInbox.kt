package com.pollisnative.share

import android.content.ContentResolver
import android.content.Context
import android.content.Intent
import android.net.Uri
import android.os.Build
import android.os.ParcelFileDescriptor
import android.os.Process
import android.provider.OpenableColumns
import android.system.Os
import android.util.Log
import org.json.JSONArray
import org.json.JSONObject
import java.io.File
import java.io.FileInputStream
import java.util.UUID

/**
 * What another app shared, held for the app until the user picks the chats (#2100).
 *
 * [ShareIntakeActivity] copies each shared file into `cacheDir/pollis-share/<id>/` while it still
 * holds the read grant on its `content://` URI (the grant ends when that activity finishes), and
 * writes a `share.json` beside them. A `pending` file names the newest share; the app takes it
 * after unlock ([take]) and deletes the directory once the share is sent or cancelled
 * ([discard]). Only one share waits: a newer one replaces it.
 *
 * The copies are transient plaintext in the app's private cache, like a picked attachment: the
 * core stores them in the encrypted database before sending, and they are deleted then, on
 * cancel, at sign-out, and by [sweep] after a day.
 */
object ShareInbox {
  private const val TAG = "PollisShare"
  private const val DIR = "pollis-share"
  private const val PENDING = "pending"
  private const val MANIFEST = "share.json"

  /** The existing attachment cap (`R2_MEDIA_MAX_BYTES`). */
  const val MAX_BYTES: Long = 100L * 1024 * 1024
  /** Most files one share carries (pollis-core `share::send::MAX_ITEMS`). */
  const val MAX_ITEMS = 10
  /** A share left waiting this long is dropped. */
  private const val STALE_MS = 24L * 60 * 60 * 1000

  private fun root(context: Context) = File(context.cacheDir, DIR)

  /** Copy what [intent] shares and make it the pending share. Never throws. */
  fun intake(context: Context, intent: Intent): Boolean {
    return try {
      sweep(context)
      val id = UUID.randomUUID().toString()
      val dir = File(root(context), id)
      if (!dir.mkdirs()) {
        return false
      }
      val manifest = collect(context, intent, dir)
      manifest.put("id", id)
      manifest.put("createdAt", System.currentTimeMillis())
      File(dir, MANIFEST).writeText(manifest.toString())
      val previous = pendingId(context)
      File(root(context), PENDING).writeText(id)
      if (previous != null && previous != id) {
        File(root(context), previous).deleteRecursively()
      }
      true
    } catch (e: Exception) {
      Log.w(TAG, "share intake failed", e)
      false
    }
  }

  private fun collect(context: Context, intent: Intent, dir: File): JSONObject {
    val out = JSONObject()
    intent.getCharSequenceExtra(Intent.EXTRA_TEXT)?.toString()?.takeIf { it.isNotBlank() }?.let {
      out.put("text", it)
    }
    intent.getStringExtra(Intent.EXTRA_SUBJECT)?.takeIf { it.isNotBlank() }?.let {
      out.put("subject", it)
    }
    val uris = streams(intent)
    val items = JSONArray()
    val refused = JSONArray()
    var truncated = false
    for ((index, uri) in uris.withIndex()) {
      if (items.length() >= MAX_ITEMS) {
        truncated = true
        break
      }
      val item = copy(context, uri, index, intent.type, dir)
      if (item.has("path")) {
        items.put(item)
      } else {
        refused.put(item)
      }
    }
    out.put("items", items)
    out.put("refused", refused)
    out.put("truncated", truncated)
    return out
  }

  @Suppress("DEPRECATION")
  private fun streams(intent: Intent): List<Uri> {
    val single: Uri? = if (Build.VERSION.SDK_INT >= 33) {
      intent.getParcelableExtra(Intent.EXTRA_STREAM, Uri::class.java)
    } else {
      intent.getParcelableExtra(Intent.EXTRA_STREAM)
    }
    val many: List<Uri>? = if (Build.VERSION.SDK_INT >= 33) {
      intent.getParcelableArrayListExtra(Intent.EXTRA_STREAM, Uri::class.java)
    } else {
      intent.getParcelableArrayListExtra(Intent.EXTRA_STREAM)
    }
    return when (intent.action) {
      Intent.ACTION_SEND_MULTIPLE -> many.orEmpty()
      else -> listOfNotNull(single)
    }
  }

  /**
   * Whether a shared URI may be read. Only another app's `content://` provider: a `file://` URI, or
   * one served by this app's own providers, could name Pollis's private files, and a share must
   * never send those. This activity runs as Pollis, so its own providers would answer it even
   * unexported: ownership is decided by the provider's UID, never by the authority's spelling, and
   * a `user@authority` form (which the resolver strips) is refused outright.
   */
  fun readable(context: Context, uri: Uri): Boolean {
    if (uri.scheme != ContentResolver.SCHEME_CONTENT) {
      return false
    }
    val authority = uri.authority ?: return false
    if (authority.isEmpty() || authority.contains('@')) {
      return false
    }
    if (authority.startsWith(context.packageName)) {
      return false
    }
    val provider = try {
      context.packageManager.resolveContentProvider(authority, 0)
    } catch (e: Exception) {
      null
    } ?: return false
    return provider.applicationInfo.uid != Process.myUid() && provider.packageName != context.packageName
  }

  /**
   * Whether an opened descriptor is one of Pollis's own files: a second line behind [readable], for
   * a provider that hands back a path it was tricked into opening. Pipes and sockets pass.
   */
  fun ownsFile(context: Context, pfd: ParcelFileDescriptor): Boolean {
    val target = try {
      Os.readlink("/proc/self/fd/${pfd.fd}")
    } catch (e: Exception) {
      return false
    }
    if (!target.startsWith("/")) {
      return false
    }
    val mine = listOfNotNull(
      context.applicationInfo.dataDir,
      context.filesDir?.canonicalPath,
      context.cacheDir?.canonicalPath,
      context.dataDir?.canonicalPath,
      context.externalCacheDir?.canonicalPath,
      context.getExternalFilesDir(null)?.canonicalPath,
      "/data/data/${context.packageName}",
      "/data/user/0/${context.packageName}",
      "/data/user_de/0/${context.packageName}",
    )
    val path = try { File(target).canonicalPath } catch (e: Exception) { target }
    return mine.any { dir -> path == dir || path.startsWith("$dir/") }
  }

  /** A display name safe to use as a file name: no path, no control characters, bounded. */
  fun safeName(raw: String?, index: Int): String {
    val base = raw?.substringAfterLast('/')?.substringAfterLast('\\')
      ?.filter { it >= ' ' && it != '/' && it != '\\' && it != ':' }
      ?.trim()
      ?.trimStart('.')
      ?.take(120)
    return if (base.isNullOrEmpty()) "file-${index + 1}" else base
  }

  private fun copy(context: Context, uri: Uri, index: Int, intentType: String?, dir: File): JSONObject {
    val item = JSONObject()
    val resolver = context.contentResolver
    var name: String? = null
    var size: Long = -1
    try {
      resolver.query(uri, arrayOf(OpenableColumns.DISPLAY_NAME, OpenableColumns.SIZE), null, null, null)?.use { c ->
        if (c.moveToFirst()) {
          val n = c.getColumnIndex(OpenableColumns.DISPLAY_NAME)
          val s = c.getColumnIndex(OpenableColumns.SIZE)
          if (n >= 0 && !c.isNull(n)) name = c.getString(n)
          if (s >= 0 && !c.isNull(s)) size = c.getLong(s)
        }
      }
    } catch (e: Exception) {
      Log.w(TAG, "no metadata for a shared item", e)
    }
    val fileName = safeName(name, index)
    item.put("name", fileName)
    val mime = (try { resolver.getType(uri) } catch (e: Exception) { null })
      ?: intentType?.takeIf { !it.contains('*') }
      ?: "application/octet-stream"
    item.put("mimeType", mime)
    if (!readable(context, uri)) {
      item.put("reason", "unreadable")
      return item
    }
    if (size > MAX_BYTES) {
      item.put("reason", "too_large")
      return item
    }
    val target = File(dir, "${index + 1}-$fileName")
    var copied = 0L
    try {
      val pfd = resolver.openFileDescriptor(uri, "r") ?: run {
        item.put("reason", "unreadable")
        return item
      }
      if (ownsFile(context, pfd)) {
        pfd.close()
        item.put("reason", "unreadable")
        return item
      }
      ParcelFileDescriptor.AutoCloseInputStream(pfd).use { src ->
        target.outputStream().use { dst ->
          val buf = ByteArray(256 * 1024)
          while (true) {
            val n = src.read(buf)
            if (n < 0) break
            copied += n
            // The provider may not have said, or said wrong: stop at the cap, not after it.
            if (copied > MAX_BYTES) {
              item.put("reason", "too_large")
              break
            }
            dst.write(buf, 0, n)
          }
        }
      }
    } catch (e: Exception) {
      Log.w(TAG, "a shared item could not be read", e)
      item.put("reason", "unreadable")
    }
    if (item.has("reason")) {
      target.delete()
      return item
    }
    item.put("path", target.absolutePath)
    item.put("size", copied)
    return item
  }

  private fun pendingId(context: Context): String? {
    val f = File(root(context), PENDING)
    return if (f.isFile) f.readText().trim().takeIf { it.isNotEmpty() } else null
  }

  /**
   * The pending share's manifest. It stays pending until [discard], so a crash or a JS reload
   * before the share is sent or cancelled offers it again (the app skips one it already holds).
   */
  fun take(context: Context): String? {
    val id = pendingId(context) ?: return null
    val manifest = File(File(root(context), id), MANIFEST)
    return if (manifest.isFile) manifest.readText() else null
  }

  /** Delete one share's files, and stop offering it. */
  fun discard(context: Context, id: String) {
    if (id.isEmpty() || id.contains('/') || id.contains("..")) {
      return
    }
    if (pendingId(context) == id) {
      File(root(context), PENDING).delete()
    }
    File(root(context), id).deleteRecursively()
  }

  /** Delete every share (sign-out, account removal). */
  fun discardAll(context: Context) {
    root(context).deleteRecursively()
  }

  /** Drop shares left waiting past [STALE_MS]. */
  private fun sweep(context: Context) {
    val now = System.currentTimeMillis()
    root(context).listFiles()?.forEach { f ->
      if (f.isDirectory && now - f.lastModified() > STALE_MS) {
        if (pendingId(context) == f.name) {
          File(root(context), PENDING).delete()
        }
        f.deleteRecursively()
      }
    }
  }
}
