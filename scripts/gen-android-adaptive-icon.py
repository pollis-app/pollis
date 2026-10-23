#!/usr/bin/env python3
"""Render the Android adaptive-icon foreground + monochrome layers from the logo SVG.

    uv run --with resvg-py --with pillow scripts/gen-android-adaptive-icon.py \
        assets/pollis-logo.svg <foreground.png> <monochrome.png>

An adaptive layer is 108dp square; the launcher shows the central 72dp through
its mask (circle, squircle, ...) and only the central 66dp circle is safe on
every mask. The glyph is drawn straight from the vector, without the amber
tile (the amber is the layer's `backgroundColor` in mobile/app.json), scaled
about the artwork's own centre until its farthest pixel sits on the safe-zone
circle (MAX_RADIUS_DP), so no mask clips it. Nothing is
upscaled from a bitmap, and no anti-aliased edge of the tile is left behind.

#1406: the old ImageMagick step keyed the amber out of a bitmap and then
trimmed, but the tile's anti-aliased edge survived the key, so the trim kept
the whole 870px tile and the lock came out 58dp tall / 38dp wide, and the
themed (monochrome) icon carried a faint outline of the tile.
"""
import io
import math
import sys

import resvg_py
from PIL import Image

# Output canvas, px (= 108dp).
SIZE = 1024
# Farthest glyph pixel from the layer's centre, in dp: the radius of the 66dp
# safe zone, so no mask shape can clip it. The lock is tall, so this is set by
# the body's lower corners, and the glyph comes out ~62dp tall.
MAX_RADIUS_DP = 33
# The tile colour in the source SVG; it is dropped from the foreground.
TILE_FILL = "fill: rgb(250, 191, 90)"


def render(svg: str, view: tuple[float, float, float, float]) -> Image.Image:
    x, y, w, h = view
    doc = svg.replace('viewBox="0 0 1024 1024"', f'viewBox="{x} {y} {w} {h}"', 1)
    png = resvg_py.svg_to_bytes(svg_string=doc, width=SIZE, height=SIZE)
    return Image.open(io.BytesIO(bytes(png))).convert("RGBA")


def main() -> None:
    src, fg_out, mono_out = sys.argv[1:4]
    svg = open(src, encoding="utf-8").read()
    if 'viewBox="0 0 1024 1024"' not in svg or TILE_FILL not in svg:
        sys.exit(f"{src}: expected a 1024 viewBox and the amber tile fill; update this script")
    glyph_svg = svg.replace(TILE_FILL, "fill: none")

    # Measure the glyph at the artwork's own scale: the farthest painted pixel
    # from the artwork's centre, in viewBox units.
    probe = render(glyph_svg, (0, 0, 1024, 1024)).getchannel("A").load()
    reach = max(
        (math.hypot(x + 0.5 - 512, y + 0.5 - 512) for y in range(SIZE) for x in range(SIZE) if probe[x, y] > 0),
        default=0,
    )
    if reach == 0:
        sys.exit(f"{src}: rendered nothing once the tile was removed")

    # Zoom about the artwork's centre (its composition is kept, as on iOS): a
    # square window that puts that farthest pixel MAX_RADIUS_DP from the centre.
    side = reach * 108 / MAX_RADIUS_DP
    fg = render(glyph_svg, (512 - side / 2, 512 - side / 2, side, side))
    fg.save(fg_out, optimize=True)

    # Monochrome (Android 13 themed icons): only alpha is used; draw it white.
    alpha = fg.getchannel("A")
    mono = Image.new("RGBA", fg.size, (255, 255, 255, 0))
    mono.putalpha(alpha)
    mono.save(mono_out, optimize=True)

    b = alpha.getbbox()
    print(f"glyph {(b[2]-b[0])/SIZE*108:.1f} x {(b[3]-b[1])/SIZE*108:.1f} dp of 108 "
          f"({(b[2]-b[0])/SIZE:.1%} x {(b[3]-b[1])/SIZE:.1%})")


if __name__ == "__main__":
    main()
