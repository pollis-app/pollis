// The app's letter avatar (components/ui.tsx `Avatar`), drawn natively for the communication
// notification (#1599): the initial in bold at 0.38 of the size, `semantic.dim` on
// `semantic.high`, a circle for a person and a rounded square (`r.lg`) for a group. Same recipe as
// theme/derive.ts and Android's LetterAvatar.kt, so a custom accent carries over. CoreGraphics and
// CoreText only, so the macOS harness draws exactly what the extension draws.

import CoreGraphics
import CoreText
import Foundation
import ImageIO
import UniformTypeIdentifiers

enum LetterAvatar {
  static let defaultAccent: UInt32 = 0xFABF5A
  static let defaultBackground: UInt32 = 0x0A0907
  // Written by the app (modules/pollis-native/ios/PollisPushState.mm) into the App Group defaults.
  static let accentKey = "pollis.theme.accent"
  static let backgroundKey = "pollis.theme.bg"

  private static func channel(_ c: UInt32, _ shift: UInt32) -> Double {
    Double((c >> shift) & 0xFF)
  }

  // derive.ts `tier`: the accent mixed into the background lightened toward white, rounded once.
  static func tier(_ accent: UInt32, _ bg: UInt32, accentWeight: Double, lighten: Double) -> UInt32 {
    var out: UInt32 = 0
    for shift: UInt32 in [16, 8, 0] {
      let lifted = 255 * lighten + channel(bg, shift) * (1 - lighten)
      let v = channel(accent, shift) * accentWeight + lifted * (1 - accentWeight)
      out |= UInt32(max(0, min(255, v.rounded()))) << shift
    }
    return out
  }

  /// `semantic.high` (fill) and `semantic.dim` (letter), as 0xRRGGBB.
  static func colors(accent: UInt32, background: UInt32) -> (fill: UInt32, letter: UInt32) {
    (tier(accent, background, accentWeight: 0.1, lighten: 0.075),
     tier(accent, background, accentWeight: 0.3, lighten: 0.69))
  }

  static func parseHex(_ hex: String?) -> UInt32? {
    guard var h = hex?.trimmingCharacters(in: .whitespaces) else {
      return nil
    }
    if h.hasPrefix("#") {
      h.removeFirst()
    }
    guard h.count == 6, let v = UInt32(h, radix: 16) else {
      return nil
    }
    return v
  }

  /// The first user-perceived character, upper-cased; nil for a blank label.
  static func initial(_ label: String?) -> String? {
    guard let first = label?.trimmingCharacters(in: .whitespacesAndNewlines).first else {
      return nil
    }
    return String(first).uppercased(with: Locale.current)
  }

  /// The theme the app last saved, or the defaults.
  static func theme(_ defaults: UserDefaults?) -> (accent: UInt32, background: UInt32) {
    (parseHex(defaults?.string(forKey: accentKey)) ?? defaultAccent,
     parseHex(defaults?.string(forKey: backgroundKey)) ?? defaultBackground)
  }

  private static func cgColor(_ rgb: UInt32) -> CGColor {
    CGColor(
      srgbRed: channel(rgb, 16) / 255, green: channel(rgb, 8) / 255, blue: channel(rgb, 0) / 255,
      alpha: 1
    )
  }

  /// PNG bytes of the avatar, or nil when there is no letter to draw.
  static func png(label: String?, rounded: Bool, theme: (accent: UInt32, background: UInt32), size: Int = 120) -> Data? {
    guard let letter = initial(label) else {
      return nil
    }
    let (fill, fg) = colors(accent: theme.accent, background: theme.background)
    let s = CGFloat(size)
    guard let ctx = CGContext(
      data: nil, width: size, height: size, bitsPerComponent: 8, bytesPerRow: 0,
      space: CGColorSpace(name: CGColorSpace.sRGB)!,
      bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue
    ) else {
      return nil
    }
    let rect = CGRect(x: 0, y: 0, width: s, height: s)
    ctx.setFillColor(cgColor(fill))
    if rounded {
      // r.lg (14) on the in-app 40pt avatar.
      let radius = s * 14 / 40
      ctx.addPath(CGPath(roundedRect: rect, cornerWidth: radius, cornerHeight: radius, transform: nil))
    } else {
      ctx.addEllipse(in: rect)
    }
    ctx.fillPath()

    let font = CTFontCreateUIFontForLanguage(.emphasizedSystem, s * 0.38, nil)
      ?? CTFontCreateWithName("Helvetica-Bold" as CFString, s * 0.38, nil)
    let attrs: [NSAttributedString.Key: Any] = [
      NSAttributedString.Key(kCTFontAttributeName as String): font,
      NSAttributedString.Key(kCTForegroundColorAttributeName as String): cgColor(fg),
    ]
    let line = CTLineCreateWithAttributedString(NSAttributedString(string: letter, attributes: attrs))
    let glyphs = CTLineGetBoundsWithOptions(line, .useGlyphPathBounds)
    ctx.textPosition = CGPoint(x: (s - glyphs.width) / 2 - glyphs.minX, y: (s - glyphs.height) / 2 - glyphs.minY)
    CTLineDraw(line, ctx)

    guard let image = ctx.makeImage() else {
      return nil
    }
    let out = NSMutableData()
    guard let dest = CGImageDestinationCreateWithData(out, UTType.png.identifier as CFString, 1, nil) else {
      return nil
    }
    CGImageDestinationAddImage(dest, image, nil)
    guard CGImageDestinationFinalize(dest) else {
      return nil
    }
    return out as Data
  }
}
