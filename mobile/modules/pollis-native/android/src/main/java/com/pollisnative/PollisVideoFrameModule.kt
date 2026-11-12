package com.pollisnative

import android.graphics.Bitmap
import android.media.MediaMetadataRetriever
import android.net.Uri
import android.os.Build
import android.util.Base64
import com.facebook.react.bridge.Promise
import com.facebook.react.bridge.ReactApplicationContext
import com.facebook.react.bridge.ReactContextBaseJavaModule
import com.facebook.react.bridge.ReactMethod
import java.io.ByteArrayOutputStream
import java.util.concurrent.Executors
import kotlin.math.max
import kotlin.math.min
import kotlin.math.roundToInt

/**
 * A video's still frame from the platform decoder (#1837): MediaMetadataRetriever, rotated as the
 * track says, scaled to fit maxEdge, returned as base64 JPEG. In memory only: no file is written.
 * JS side: `NativeModules.PollisVideoFrame.frame(uri, maxEdge)` (`lib/media/videoFrame.ts`); the
 * iOS twin is ios/PollisVideoFrame.mm.
 */
class PollisVideoFrameModule(context: ReactApplicationContext) : ReactContextBaseJavaModule(context) {
  override fun getName(): String = NAME

  @ReactMethod
  fun frame(uri: String, maxEdge: Double, promise: Promise) {
    // Decoding takes a while; never on the native-modules thread.
    worker.execute {
      try {
        promise.resolve(encode(decode(uri, if (maxEdge > 0) maxEdge.roundToInt() else 640)))
      } catch (e: Throwable) {
        promise.reject("E_VIDEO_FRAME", e.message ?: "no frame", e)
      }
    }
  }

  private fun decode(uri: String, edge: Int): Bitmap {
    val retriever = MediaMetadataRetriever()
    try {
      val parsed = Uri.parse(uri)
      if (parsed.scheme == null) {
        retriever.setDataSource(uri)
      } else {
        retriever.setDataSource(reactApplicationContext, parsed)
      }
      // Half a second in (past a fade from black), or a tenth of a shorter clip; desktop does the same.
      val durationMs = retriever.extractMetadata(MediaMetadataRetriever.METADATA_KEY_DURATION)?.toLongOrNull() ?: 0L
      val atUs = if (durationMs > 0) min(500_000L, durationMs * 100L) else 0L
      val option = MediaMetadataRetriever.OPTION_CLOSEST_SYNC
      val frame = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O_MR1) {
        retriever.getScaledFrameAtTime(atUs, option, edge, edge)
      } else {
        retriever.getFrameAtTime(atUs, option)?.let { fit(it, edge) }
      }
      return frame ?: throw IllegalStateException("no frame")
    } finally {
      retriever.release()
    }
  }

  private fun fit(bitmap: Bitmap, edge: Int): Bitmap {
    val longest = max(bitmap.width, bitmap.height)
    if (longest <= edge) {
      return bitmap
    }
    val scale = edge.toFloat() / longest
    return Bitmap.createScaledBitmap(
      bitmap,
      max(1, (bitmap.width * scale).roundToInt()),
      max(1, (bitmap.height * scale).roundToInt()),
      true,
    )
  }

  private fun encode(bitmap: Bitmap): String {
    val out = ByteArrayOutputStream()
    if (!bitmap.compress(Bitmap.CompressFormat.JPEG, 80, out)) {
      throw IllegalStateException("jpeg encode failed")
    }
    return Base64.encodeToString(out.toByteArray(), Base64.NO_WRAP)
  }

  companion object {
    const val NAME = "PollisVideoFrame"
    private val worker = Executors.newSingleThreadExecutor()
  }
}
