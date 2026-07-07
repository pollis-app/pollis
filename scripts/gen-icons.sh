#!/usr/bin/env bash
#
# Regenerate every platform icon from the single source of truth.
#
#   Source SVG : assets/pollis-logo.svg   (edit this, then re-run)
#   Master PNG : assets/new-icon.png      (1024² designer export of the SVG)
#
# One command re-derives desktop (.icns/.ico/PNG/Windows tiles), iOS, Android
# (adaptive foreground + background + Material-You monochrome), the tray set,
# the mobile Expo assets, and the web/in-app favicons. A future logo swap is
# "drop the new export in, run this."
#
# Requires: ImageMagick (`magick`) and the Tauri CLI (`pnpm tauri`).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

MASTER="assets/new-icon.png"   # 1024² amber rounded square, transparent corners, dark lock
AMBER="#FABF5A"                # brand background
SPLASH_BG="#0a0907"            # dark splash / dark-mode substrate
NOTIF_RED="#E5484D"            # unread notification dot

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

echo "▸ deriving master renders from $MASTER"
# The mark exactly as designed (amber rounded square, transparent corners).
cp "$MASTER" "$WORK/mark.png"
# Amber edge-to-edge (fills the transparent corners) — for platforms that
# apply their own mask (iOS) or reject alpha. `-alpha off` drops the channel
# entirely: the App Store rejects any alpha on the 1024² marketing icon.
magick "$WORK/mark.png" -background "$AMBER" -flatten -alpha off "$WORK/fullbleed.png"
# Key out the amber → just the dark lock glyph on transparency.
magick "$WORK/mark.png" -fuzz 25% -transparent "$AMBER" "$WORK/fg.png"
# Trimmed + safe-zone-padded foreground for Android adaptive (≈64% fill).
magick "$WORK/fg.png" -trim +repage -resize 660x660 \
  -background none -gravity center -extent 1024x1024 "$WORK/fg-adaptive.png"
# Monochrome layers (keep alpha, recolor the glyph).
magick "$WORK/fg-adaptive.png" -channel RGB -fill white -colorize 100 +channel "$WORK/mono-white.png"
magick "$WORK/fg.png"          -channel RGB -fill black -colorize 100 +channel "$WORK/mono-black.png"
# Solid amber background layer for Android adaptive.
magick -size 1024x1024 "xc:$AMBER" "$WORK/android-bg.png"

echo "▸ tauri icon → src-tauri/icons (desktop + iOS + Android)"
cat > "$WORK/manifest.json" <<JSON
{
  "default": "mark.png",
  "bg_color": "$AMBER",
  "android_bg": "android-bg.png",
  "android_fg": "fg-adaptive.png",
  "android_fg_scale": 100,
  "android_monochrome": "mono-white.png"
}
JSON
pnpm tauri icon "$WORK/manifest.json" -o src-tauri/icons >/dev/null
# tauri icon flattens the iOS set over bg_color but leaves an (opaque) alpha
# channel; App Store Connect rejects any alpha on app icons. Drop it.
magick mogrify -background "$AMBER" -alpha remove -alpha off src-tauri/icons/ios/*.png
# tauri.conf references AppIcon.icns; mirror the generated icon.icns to it.
cp src-tauri/icons/icon.icns src-tauri/icons/AppIcon.icns
# tauri icon skips these non-standard square sizes — regenerate so none go stale.
# generate_context! embeds 32/64/128/128@2x and panics unless they are true RGBA,
# so force PNG32: (plain -resize can emit a palette/RGB PNG the macro rejects).
for s in 32 64 128 256 512; do
  magick "$WORK/mark.png" -resize "${s}x${s}" "PNG32:src-tauri/icons/${s}x${s}.png"
done
magick "$WORK/mark.png" -resize 256x256 PNG32:src-tauri/icons/128x128@2x.png
# Android adaptive background colour (legacy resource) → brand amber, not #fff.
cat > src-tauri/icons/android/values/ic_launcher_background.xml <<XML
<?xml version="1.0" encoding="utf-8"?>
<resources>
  <color name="ic_launcher_background">$AMBER</color>
</resources>
XML
# Legacy in-repo copies kept in sync.
magick "$WORK/mark.png" -resize 512x512 src-tauri/icons/pollis-logo.png
magick "$WORK/mark.png" -resize 256x256 src-tauri/icons/pollis.png

echo "▸ tray icons"
# Tauri's include_image! macro demands true 8-bit RGBA — force it with PNG32:
# (a grayscale/RGB or palette PNG panics the proc-macro at compile time).
# Windows/Linux: full-colour mark.
magick "$WORK/mark.png" -resize 64x64 PNG32:src-tauri/icons/tray-default.png
# Unread variant: red dot, top-right.
magick src-tauri/icons/tray-default.png -fill "$NOTIF_RED" -stroke none \
  -draw "circle 48,16 48,4" PNG32:src-tauri/icons/tray-notification.png
# macOS menu bar: black glyph on transparency — macOS tints it as a template.
magick "$WORK/mono-black.png" -resize 22x22 PNG32:src-tauri/icons/tray-mac.png

echo "▸ mobile Expo assets"
magick "$WORK/fullbleed.png" -resize 1024x1024 mobile/assets/icon.png
cp "$WORK/fg-adaptive.png"  mobile/assets/adaptive-icon.png
cp "$WORK/mono-white.png"   mobile/assets/adaptive-monochrome.png
cp "$WORK/mark.png"         mobile/assets/splash-icon.png
magick "$WORK/mark.png" -resize 48x48 mobile/assets/favicon.png
# iOS 18 light / dark / tinted.
cp "$WORK/fullbleed.png" mobile/assets/ios-light.png
magick "$WORK/fg.png" -channel RGB -fill "$AMBER" -colorize 100 +channel \
  -background "$SPLASH_BG" -flatten -alpha off mobile/assets/ios-dark.png
magick "$WORK/fg.png" -channel RGB -fill white -colorize 100 +channel \
  -background "#1c1c1e" -flatten -alpha off mobile/assets/ios-tinted.png

echo "▸ web + in-app favicons / logos"
magick "$WORK/mark.png" -resize 512x512 frontend/public/favicon.png
magick "$WORK/mark.png" -resize 256x256 frontend/public/windows-icon-default.png
magick "$WORK/mark.png" -resize 256x256 -fill "$NOTIF_RED" \
  -draw "circle 192,64 192,32" frontend/public/windows-icon-notification.png
cp frontend/public/windows-icon-notification.png assets/windows-icon-notification.png
magick "$WORK/mark.png" -resize 862x862 frontend/src/assets/images/logo-universal.png
magick "$WORK/mark.png" -resize 862x862 frontend/src/assets/images/LogoBigMono.png

echo "✓ icons regenerated"
