#!/usr/bin/env python3
"""Render the Android adaptive-icon foreground + monochrome layers from the logo SVG.

    uv run --with resvg-py --with pillow scripts/gen-android-adaptive-icon.py \
        assets/pollis-logo.svg <foreground.png> <monochrome.png>

An adaptive layer is 108dp square; the launcher shows the central 72dp through
its mask (circle, squircle, ...) and only the central 66dp circle is safe on
every mask. The logo is rendered straight from the vector, tile and all, and
scaled about the artwork's own centre until the glyph's farthest pixel sits on
the safe-zone circle (MAX_RADIUS_DP), so no mask clips it. The amber then
becomes transparency (it is the layer's `backgroundColor` in mobile/app.json):
the artwork uses exactly two colours, so each pixel is `t * INK + (1 - t) *
AMBER` and `t` is its alpha. Over amber the layer reproduces the logo exactly,
including what the amber hides: the lock body's amber fill covers the lower end
of the shackle's left leg, so simply dropping the amber fills would expose it.
Nothing is upscaled from a bitmap, and no edge of the tile is left behind.

#1406: the old ImageMagick step keyed the amber out of a bitmap and then
trimmed, but the tile's anti-aliased edge survived the key, so the trim kept
the whole 870px tile and the lock came out 58dp tall / 38dp wide, and the
themed (monochrome) icon carried a faint outline of the tile.
"""
import io
import math
import re
import sys

import resvg_py
from PIL import Image

# Output canvas, px (= 108dp).
SIZE = 1024
# Farthest glyph pixel from the layer's centre, in dp: the radius of the 66dp
# safe zone, so no mask shape can clip it. The lock is tall, so this is set by
# the body's lower corners, and the glyph comes out ~62dp tall.
MAX_RADIUS_DP = 33
# The logo's two colours: the amber (tile and lock interior) and the ink.
AMBER = (250, 191, 90)
INK = (50, 50, 50)


def render(svg: str, view: tuple[float, float, float, float]) -> Image.Image:
    x, y, w, h = view
    doc = svg.replace('viewBox="0 0 1024 1024"', f'viewBox="{x} {y} {w} {h}"', 1)
    png = resvg_py.svg_to_bytes(svg_string=doc, width=SIZE, height=SIZE)
    return Image.open(io.BytesIO(bytes(png))).convert("RGBA")


def ink_alpha(im: Image.Image) -> Image.Image:
    """Ink coverage of each pixel of a render of the full logo (tile included)."""
    d = [a - i for a, i in zip(AMBER, INK)]
    norm = sum(v * v for v in d)
    out = []
    raw = im.tobytes()
    for i in range(0, len(raw), 4):
        r, g, b, a = raw[i : i + 4]
        if a < 255:
            # Outside the tile, or its anti-aliased rim: no ink reaches there.
            out.append(0)
            continue
        t = ((AMBER[0] - r) * d[0] + (AMBER[1] - g) * d[1] + (AMBER[2] - b) * d[2]) / norm
        out.append(max(0, min(255, round(t * 255))))
    alpha = Image.new("L", im.size)
    alpha.putdata(out)
    return alpha


def main() -> None:
    src, fg_out, mono_out = sys.argv[1:4]
    svg = open(src, encoding="utf-8").read()
    colours = set(re.findall(r"(?:fill|stroke): rgb\(([^)]*)\)", svg))
    if 'viewBox="0 0 1024 1024"' not in svg or colours != {"250, 191, 90", "50, 50, 50"}:
        sys.exit(f"{src}: expected a 1024 viewBox and only the amber + ink colours; update this script")

    # Measure the glyph at the artwork's own scale: the farthest ink pixel from
    # the artwork's centre, in viewBox units.
    probe = ink_alpha(render(svg, (0, 0, 1024, 1024))).load()
    reach = max(
        (math.hypot(x + 0.5 - 512, y + 0.5 - 512) for y in range(SIZE) for x in range(SIZE) if probe[x, y] > 0),
        default=0,
    )
    if reach == 0:
        sys.exit(f"{src}: rendered no ink")

    # Zoom about the artwork's centre (its composition is kept, as on iOS): a
    # square window that puts that farthest pixel MAX_RADIUS_DP from the centre.
    side = reach * 108 / MAX_RADIUS_DP
    alpha = ink_alpha(render(svg, (512 - side / 2, 512 - side / 2, side, side)))
    fg = Image.new("RGBA", alpha.size, INK + (0,))
    fg.putalpha(alpha)
    fg.save(fg_out, optimize=True)

    # Monochrome (Android 13 themed icons): only alpha is used; draw it white.
    mono = Image.new("RGBA", fg.size, (255, 255, 255, 0))
    mono.putalpha(alpha)
    mono.save(mono_out, optimize=True)

    b = alpha.getbbox()
    print(f"glyph {(b[2]-b[0])/SIZE*108:.1f} x {(b[3]-b[1])/SIZE*108:.1f} dp of 108 "
          f"({(b[2]-b[0])/SIZE:.1%} x {(b[3]-b[1])/SIZE:.1%})")


if __name__ == "__main__":
    main()
