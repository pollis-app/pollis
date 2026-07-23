#!/usr/bin/env bash
# Render a Learn animation to the shipping media artifacts, NO audio track.
#
# Produces, under website/learn/media/<slug>.*:
#   <slug>.mp4   H.264 (universal fallback)
#   <slug>.webm  VP9   (smaller, preferred by <source> order)
#   <slug>.jpg   poster frame (first frame)
# The .vtt caption track is authored by hand from learn/manim/scripts/<slug>.md
# (timed to the scene beats) and is NOT generated here.
#
# Usage:  learn/manim/render.sh <SceneClass> <slug> [quality]
#   e.g.  learn/manim/render.sh MerkleTrees merkle-trees h
# quality: l|m|h|k  (default h = 1080p60 is overkill for flat vector; m = 720p30)
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
repo="$(cd "$here/../.." && pwd)"
scene_class="${1:?scene class, e.g. MerkleTrees}"
slug="${2:?output slug, e.g. merkle-trees}"
q="${3:-m}"

manim="$here/.venv/bin/manim"
src="$here/scenes/${slug//-/_}.py"
out="$repo/website/learn/media"
mkdir -p "$out"

# Manim writes its media/ tree relative to CWD — pin it to learn/manim so the
# find below is deterministic regardless of where the script was invoked from.
cd "$here"

echo "▶ rendering $scene_class from $src at -q$q (no audio)"
"$manim" "-q$q" --format=mp4 --disable_caching "$src" "$scene_class" -o "$slug"

# Manim drops the mp4 under media/videos/<file>/<res>/<slug>.mp4 — find the newest.
mp4="$(find "$here/media/videos" -name "${slug}.mp4" -printf '%T@ %p\n' | sort -nr | head -1 | cut -d' ' -f2-)"
[ -n "$mp4" ] || { echo "render produced no mp4"; exit 1; }

cp "$mp4" "$out/$slug.mp4"

echo "▶ transcoding VP9 webm"
ffmpeg -y -loglevel error -i "$out/$slug.mp4" -an \
  -c:v libvpx-vp9 -b:v 0 -crf 34 -row-mt 1 "$out/$slug.webm"

echo "▶ extracting poster frame (~35% in — frame 0 is usually blank)"
dur="$(ffprobe -v error -show_entries format=duration -of csv=p=0 "$out/$slug.mp4")"
ts="$(awk -v d="$dur" 'BEGIN { printf "%.2f", d * 0.35 }')"
ffmpeg -y -loglevel error -ss "$ts" -i "$out/$slug.mp4" -frames:v 1 "$out/$slug.jpg"

echo "✓ wrote:"
ls -la "$out/$slug".{mp4,webm,jpg}
echo "  reminder: hand-author $out/$slug.vtt from learn/manim/scripts/$slug.md"
