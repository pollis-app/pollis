#!/usr/bin/env bash
# Visual before/after comparison for Maestro screenshot galleries (macOS).
#
# Pairs the screenshots in BEFORE_DIR and AFTER_DIR by file name, and writes:
#   <out>/side/<name>.png   one labelled image per pair: BEFORE | AFTER
#   <out>/index.html        the contact sheet: every pair side by side, with a
#                           status per row (changed / identical / only before /
#                           only after) and links to the full-size shots
#   <out>/before/, after/   copies of the inputs, so the directory is
#                           self-contained (zip it, open it anywhere)
#   <out>/summary.txt       one line per pair: status<TAB>name
#
# Pairing: exact file name first; a file left over on either side is then
# matched by its numeric prefix (tour-07-groups-empty.png <-> tour-07-groups.png),
# so renaming a screen's slug does not drop it from the comparison.
#
# Usage:
#   mobile/scripts/visual-compare.sh BEFORE_DIR AFTER_DIR [OUT_DIR]
# Env:
#   PREFIX   file-name prefix to compare (default "tour-"; "" = every *.png)
#   HEIGHT   pixel height each shot is scaled to in side/ (default 1400)
#   NAME     suffix for the default output dir (e.g. NAME=ios-checkpoint-1)
# Default OUT_DIR: mobile/.maestro/artifacts/compare/<YYYY-MM-DD_HHMMSS>[-NAME]/
# (under the gitignored artifacts tree — the PNGs are never committed).
#
# Examples:
#   mobile/scripts/visual-compare.sh \
#     mobile/.maestro/artifacts/baseline-before/ios \
#     mobile/.maestro/artifacts/2026-10-09/ios
#   NAME=android mobile/scripts/visual-compare.sh \
#     mobile/.maestro/artifacts/baseline-before/android \
#     mobile/.maestro/artifacts/2026-10-09/android
#   open mobile/.maestro/artifacts/compare/<run>/index.html
#
# Needs only what ships with macOS + Xcode: bash, md5, and `swiftc` for the
# composited PNGs. Without a working swiftc it still writes index.html (the
# browser lays the pairs side by side) and says so.
set -euo pipefail

BEFORE="${1:?usage: visual-compare.sh BEFORE_DIR AFTER_DIR [OUT_DIR]}"
AFTER="${2:?usage: visual-compare.sh BEFORE_DIR AFTER_DIR [OUT_DIR]}"
[ -d "$BEFORE" ] || { echo "not a directory: $BEFORE" >&2; exit 1; }
[ -d "$AFTER" ] || { echo "not a directory: $AFTER" >&2; exit 1; }
BEFORE="$(cd "$BEFORE" && pwd)"
AFTER="$(cd "$AFTER" && pwd)"

PREFIX="${PREFIX-tour-}"
HEIGHT="${HEIGHT:-1400}"
MOBILE="$(cd "$(dirname "$0")/.." && pwd)"
if [ -n "${3:-}" ]; then
  OUT="$3"
else
  OUT="$MOBILE/.maestro/artifacts/compare/$(date +%Y-%m-%d_%H%M%S)${NAME:+-$NAME}"
fi
mkdir -p "$OUT/before" "$OUT/after" "$OUT/side"
OUT="$(cd "$OUT" && pwd)"

# --- collect -----------------------------------------------------------------
list_pngs() { (cd "$1" && ls -1 2>/dev/null | grep -E "^${PREFIX}.*\.png$" | sort) || true; }
# numeric key: "tour-07-groups.png" -> "tour-07"; no number -> whole name
num_key() { echo "$1" | sed -E "s/^(${PREFIX}[0-9]+)[-_.].*/\1/"; }

B_LIST="$(list_pngs "$BEFORE")"
A_LIST="$(list_pngs "$AFTER")"
[ -n "$B_LIST$A_LIST" ] || { echo "no ${PREFIX}*.png in either directory" >&2; exit 1; }

# PAIRS: lines of "before_name|after_name" (either may be empty)
PAIRS=""
B_LEFT=""
while IFS= read -r b; do
  [ -n "$b" ] || continue
  if [ -f "$AFTER/$b" ]; then
    PAIRS+="$b|$b"$'\n'
  else
    B_LEFT+="$b"$'\n'
  fi
done <<< "$B_LIST"
A_LEFT=""
while IFS= read -r a; do
  [ -n "$a" ] || continue
  [ -f "$BEFORE/$a" ] || A_LEFT+="$a"$'\n'
done <<< "$A_LIST"
# second pass: match leftovers by numeric prefix
while IFS= read -r b; do
  [ -n "$b" ] || continue
  k="$(num_key "$b")"; match=""
  if [ "$k" != "$b" ]; then
    match="$(printf '%s' "$A_LEFT" | grep -E "^${k}[-_.]" | head -1 || true)"
  fi
  if [ -n "$match" ]; then
    PAIRS+="$b|$match"$'\n'
    A_LEFT="$(printf '%s' "$A_LEFT" | grep -vxF "$match" || true)"
    [ -n "$A_LEFT" ] && A_LEFT+=$'\n'
  else
    PAIRS+="$b|"$'\n'
  fi
done <<< "$B_LEFT"
while IFS= read -r a; do
  [ -n "$a" ] || continue
  PAIRS+="|$a"$'\n'
done <<< "$A_LEFT"
# sort rows by whichever name is present
PAIRS="$(printf '%s' "$PAIRS" | awk -F'|' 'NF{k=($1!=""?$1:$2); print k"|"$0}' | sort | cut -d'|' -f2-)"

# --- composite helper (Swift, compiled once into a cache) -------------------
SWIFT_SRC="$OUT/.sidebyside.swift"
cat > "$SWIFT_SRC" <<'SWIFT'
import AppKit
// args: height, then triples: before|"" after|"" out label
let a = CommandLine.arguments
let H = CGFloat(Double(a[1]) ?? 1400)
func load(_ p: String) -> CGImage? {
  guard !p.isEmpty, let s = CGImageSourceCreateWithURL(URL(fileURLWithPath: p) as CFURL, nil) else { return nil }
  return CGImageSourceCreateImageAtIndex(s, 0, nil)
}
let bg = NSColor(calibratedWhite: 0.08, alpha: 1), fg = NSColor(calibratedWhite: 0.92, alpha: 1)
let dim = NSColor(calibratedWhite: 0.55, alpha: 1)
var i = 2
while i + 3 < a.count {
  let bi = load(a[i]), ai = load(a[i+1]), out = a[i+2], label = a[i+3]; i += 4
  func w(_ img: CGImage?) -> CGFloat { guard let g = img else { return H * 0.46 }; return (CGFloat(g.width) / CGFloat(g.height) * H).rounded() }
  let pad: CGFloat = 24, head: CGFloat = 84
  let W = pad * 3 + w(bi) + w(ai), TH = head + H + pad
  guard let rep = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: Int(W), pixelsHigh: Int(TH), bitsPerSample: 8,
      samplesPerPixel: 4, hasAlpha: true, isPlanar: false, colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0) else { continue }
  NSGraphicsContext.saveGraphicsState()
  NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: rep)
  bg.setFill(); NSRect(x: 0, y: 0, width: W, height: TH).fill()
  let font = NSFont.boldSystemFont(ofSize: 30), small = NSFont.systemFont(ofSize: 22)
  func text(_ s: String, _ x: CGFloat, _ f: NSFont, _ c: NSColor) {
    (s as NSString).draw(at: NSPoint(x: x, y: TH - head + 22), withAttributes: [.font: f, .foregroundColor: c])
  }
  let cols: [(CGImage?, String, CGFloat)] = [(bi, "BEFORE", pad), (ai, "AFTER", pad * 2 + w(bi))]
  for (img, tag, x) in cols {
    text(tag, x, font, fg)
    text(label, x + 130, small, dim)
    let r = NSRect(x: x, y: pad, width: w(img), height: H)
    if let g = img {
      NSGraphicsContext.current?.cgContext.interpolationQuality = .high
      NSGraphicsContext.current?.cgContext.draw(g, in: r)
    } else {
      NSColor(calibratedWhite: 0.16, alpha: 1).setFill(); r.fill()
      ("(no screenshot)" as NSString).draw(at: NSPoint(x: x + 30, y: H / 2), withAttributes: [.font: font, .foregroundColor: dim])
    }
  }
  NSGraphicsContext.restoreGraphicsState()
  try? rep.representation(using: .png, properties: [:])?.write(to: URL(fileURLWithPath: out))
}
SWIFT
CACHE="${TMPDIR:-/tmp}/pollis-visual-compare"
mkdir -p "$CACHE"
BIN="$CACHE/sidebyside-$(md5 -q "$SWIFT_SRC")"
HAVE_SWIFT=1
if [ ! -x "$BIN" ]; then
  echo "==> compiling side-by-side helper (once)"
  swiftc -O -o "$BIN" "$SWIFT_SRC" 2>"$OUT/.swiftc.log" || HAVE_SWIFT=0
fi
rm -f "$SWIFT_SRC"

# --- copy, classify, composite ------------------------------------------------
ARGS=()
SUMMARY="$OUT/summary.txt"; : > "$SUMMARY"
n_changed=0; n_same=0; n_bonly=0; n_aonly=0
ROWS=""
html_esc() { printf '%s' "$1" | sed -e 's/&/\&amp;/g' -e 's/</\&lt;/g' -e 's/>/\&gt;/g'; }
while IFS='|' read -r b a; do
  [ -n "$b$a" ] || continue
  name="${b:-$a}"; base="${name%.png}"
  bp=""; ap=""
  [ -n "$b" ] && { cp "$BEFORE/$b" "$OUT/before/$b"; bp="$BEFORE/$b"; }
  [ -n "$a" ] && { cp "$AFTER/$a" "$OUT/after/$a"; ap="$AFTER/$a"; }
  if [ -z "$b" ]; then status="only-after"; n_aonly=$((n_aonly+1))
  elif [ -z "$a" ]; then status="only-before"; n_bonly=$((n_bonly+1))
  elif [ "$(md5 -q "$bp")" = "$(md5 -q "$ap")" ]; then status="identical"; n_same=$((n_same+1))
  else status="changed"; n_changed=$((n_changed+1)); fi
  label="$base"; [ -n "$b" ] && [ -n "$a" ] && [ "$b" != "$a" ] && label="$base -> ${a%.png}"
  printf '%s\t%s\n' "$status" "$label" >> "$SUMMARY"
  ARGS+=("$bp" "$ap" "$OUT/side/$base.png" "$label")
  bimg='<div class="none">no screenshot</div>'; aimg="$bimg"
  [ -n "$b" ] && bimg="<a href=\"before/$b\"><img loading=\"lazy\" src=\"before/$b\"></a>"
  [ -n "$a" ] && aimg="<a href=\"after/$a\"><img loading=\"lazy\" src=\"after/$a\"></a>"
  ROWS+="<section id=\"$base\" class=\"$status\"><h2><a href=\"#$base\">$(html_esc "$label")</a> <span class=\"tag\">$status</span> <a class=\"side\" href=\"side/$base.png\">side-by-side png</a></h2><div class=\"pair\"><figure><figcaption>Before</figcaption>$bimg</figure><figure><figcaption>After</figcaption>$aimg</figure></div></section>"$'\n'
done <<< "$PAIRS"

if [ "$HAVE_SWIFT" = 1 ] && [ ${#ARGS[@]} -gt 0 ]; then
  echo "==> compositing $(( ${#ARGS[@]} / 4 )) pairs"
  "$BIN" "$HEIGHT" "${ARGS[@]}" || HAVE_SWIFT=0
fi
[ "$HAVE_SWIFT" = 1 ] || { echo "WARN: swiftc unavailable/failed (see $OUT/.swiftc.log) — side/ PNGs skipped; index.html still works" >&2; rmdir "$OUT/side" 2>/dev/null || true; }

TOTAL=$((n_changed+n_same+n_bonly+n_aonly))
cat > "$OUT/index.html" <<HTML
<!doctype html>
<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>Visual compare</title>
<style>
:root{--bg:#f6f6f4;--fg:#1b1b1a;--dim:#6b6b66;--card:#fff;--line:#ddd;--chg:#b45309;--same:#3f7d4a;--miss:#b42318}
@media (prefers-color-scheme:dark){:root{--bg:#121211;--fg:#ecebe7;--dim:#9a9993;--card:#1c1c1a;--line:#333;--chg:#f0a24a;--same:#7cc68a;--miss:#f27a6c}}
*{box-sizing:border-box}body{margin:0;background:var(--bg);color:var(--fg);font:15px/1.4 -apple-system,system-ui,sans-serif;padding:16px}
header{position:sticky;top:0;background:var(--bg);padding:8px 0 12px;border-bottom:1px solid var(--line);margin-bottom:16px;z-index:1}
h1{font-size:20px;margin:0 0 4px}.meta{color:var(--dim);font-size:13px;word-break:break-all}
.filters label{margin-right:14px;font-size:14px;cursor:pointer}
section{background:var(--card);border:1px solid var(--line);border-radius:10px;padding:12px;margin:0 0 16px}
h2{font-size:15px;margin:0 0 10px;display:flex;gap:10px;align-items:baseline;flex-wrap:wrap}h2 a{color:inherit;text-decoration:none}
.tag{font-size:12px;font-weight:600;padding:1px 8px;border-radius:99px;border:1px solid currentColor}
.changed .tag{color:var(--chg)}.identical .tag{color:var(--same)}.only-before .tag,.only-after .tag{color:var(--miss)}
a.side{font-size:12px;font-weight:400;color:var(--dim);margin-left:auto}
.pair{display:grid;grid-template-columns:1fr 1fr;gap:12px}figure{margin:0}figcaption{font-size:12px;color:var(--dim);margin-bottom:4px;text-transform:uppercase;letter-spacing:.05em}
img{width:100%;max-width:420px;height:auto;display:block;border:1px solid var(--line);border-radius:6px}
.none{aspect-ratio:9/19.5;max-width:420px;display:grid;place-items:center;border:1px dashed var(--line);border-radius:6px;color:var(--dim)}
body.hide-identical section.identical{display:none}
</style></head><body>
<header><h1>Visual compare — $TOTAL screens</h1>
<div class="meta">$n_changed changed · $n_same identical · $n_bonly only before · $n_aonly only after<br>before: $(html_esc "$BEFORE")<br>after: $(html_esc "$AFTER")</div>
<div class="filters"><label><input type="checkbox" onchange="document.body.classList.toggle('hide-identical',this.checked)"> hide identical</label></div></header>
$ROWS
</body></html>
HTML

echo "==> $TOTAL pairs: $n_changed changed, $n_same identical, $n_bonly only-before, $n_aonly only-after"
echo "==> index:  $OUT/index.html"
[ "$HAVE_SWIFT" = 1 ] && echo "==> pairs:  $OUT/side/"
echo "==> list:   $OUT/summary.txt"
