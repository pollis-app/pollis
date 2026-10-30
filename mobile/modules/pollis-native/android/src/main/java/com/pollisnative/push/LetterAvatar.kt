package com.pollisnative.push

import android.content.Context
import android.graphics.Bitmap
import android.graphics.Canvas
import android.graphics.Paint
import android.graphics.RectF
import android.graphics.Typeface
import java.util.Locale

/**
 * The app's letter avatar (`components/ui.tsx` `Avatar`, default variant), drawn natively for
 * notification icons (#1599): the initial in bold at 0.38 of the size, `semantic.dim` on
 * `semantic.high`, a circle for people and a rounded square (`r.lg`) for groups, as Search draws them. Mobile shows no
 * image avatars, so this is the avatar the user sees in the app too. Colours come from the same
 * two bases and recipe as `theme/derive.ts`, so a custom accent carries over.
 */
object LetterAvatar {
  const val DEFAULT_ACCENT = 0xFABF5A
  const val DEFAULT_BG = 0x0A0907

  private fun ch(c: Int, shift: Int) = (c shr shift) and 0xFF

  /**
   * derive.ts `tier`: the accent mixed into the background lightened toward white, in unrounded
   * channels and rounded once at the end, as `toHex` does.
   */
  private fun tier(accent: Int, bg: Int, accentWeight: Double, lighten: Double): Int {
    var out = 0
    for (shift in intArrayOf(16, 8, 0)) {
      val lifted = 255 * lighten + ch(bg, shift) * (1 - lighten)
      val v = ch(accent, shift) * accentWeight + lifted * (1 - accentWeight)
      out = out or (Math.round(v).toInt().coerceIn(0, 255) shl shift)
    }
    return out
  }

  /** `semantic.high` (fill) and `semantic.dim` (letter), as 0xRRGGBB. */
  fun colors(accent: Int, bg: Int): Pair<Int, Int> =
    Pair(tier(accent, bg, 0.1, 0.075), tier(accent, bg, 0.3, 0.69))

  /** The first user-perceived character, upper-cased; null for a blank label. */
  fun initial(label: String?, locale: Locale = Locale.getDefault()): String? {
    val trimmed = label?.trim().orEmpty()
    if (trimmed.isEmpty()) {
      return null
    }
    val end = trimmed.offsetByCodePoints(0, 1)
    return trimmed.substring(0, end).uppercase(locale)
  }

  fun parseHex(hex: String?): Int? {
    val h = hex?.trim()?.removePrefix("#") ?: return null
    if (h.length != 6) {
      return null
    }
    return h.toIntOrNull(16)
  }

  private const val PREFS = "pollis_push_theme"

  /** JS saves the live accent and background (`PollisPushState.setThemeColors`) for cold processes. */
  fun saveTheme(context: Context, accentHex: String, bgHex: String) {
    if (parseHex(accentHex) == null || parseHex(bgHex) == null) {
      return
    }
    context.getSharedPreferences(PREFS, Context.MODE_PRIVATE).edit()
      .putString("accent", accentHex)
      .putString("bg", bgHex)
      .apply()
  }

  private fun theme(context: Context): Pair<Int, Int> {
    val prefs = context.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
    val accent = parseHex(prefs.getString("accent", null)) ?: DEFAULT_ACCENT
    val bg = parseHex(prefs.getString("bg", null)) ?: DEFAULT_BG
    return Pair(accent, bg)
  }

  /** The avatar bitmap, or null when there is no letter to draw. */
  fun bitmap(context: Context, label: String?, rounded: Boolean): Bitmap? {
    val letter = initial(label) ?: return null
    val (accent, bg) = theme(context)
    val (fill, fg) = colors(accent, bg)
    val density = context.resources.displayMetrics.density
    val size = (48 * density).toInt().coerceIn(48, 256)
    val bmp = Bitmap.createBitmap(size, size, Bitmap.Config.ARGB_8888)
    val canvas = Canvas(bmp)
    val paint = Paint(Paint.ANTI_ALIAS_FLAG)
    paint.color = 0xFF000000.toInt() or fill
    val rect = RectF(0f, 0f, size.toFloat(), size.toFloat())
    if (rounded) {
      // r.lg (14) on the in-app 40pt avatar.
      val radius = size * 14f / 40f
      canvas.drawRoundRect(rect, radius, radius, paint)
    } else {
      canvas.drawOval(rect, paint)
    }
    paint.color = 0xFF000000.toInt() or fg
    paint.typeface = Typeface.create(Typeface.SANS_SERIF, Typeface.BOLD)
    paint.textSize = size * 0.38f
    paint.textAlign = Paint.Align.CENTER
    val y = size / 2f - (paint.descent() + paint.ascent()) / 2f
    canvas.drawText(letter, size / 2f, y, paint)
    return bmp
  }
}
