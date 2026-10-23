/*
 * The Android adaptive icon's glyph fills the safe zone and nothing else (#1406).
 *
 * A layer is 108dp; the launcher shows the central 72dp through its mask and
 * only the central 66dp circle is safe on every mask shape. The foreground and
 * monochrome layers are rendered from assets/pollis-logo.svg by
 * scripts/gen-android-adaptive-icon.py. The old pipeline left the amber
 * tile's anti-aliased edge in both layers, so the lock came out 58dp tall and
 * the themed icon carried a faint outline of the tile.
 *
 * Pinned: app.json points at both layers on the brand amber; every painted
 * pixel lies inside the 33dp safe radius (no stray outline, no clipping); the
 * glyph is at least 60dp tall; the monochrome layer is the same shape, in white.
 *
 *   cd mobile && pnpm test
 */

import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { inflateSync } from "node:zlib";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const app = JSON.parse(readFileSync(join(root, "app.json"), "utf8")).expo;
const adaptive = app.android.adaptiveIcon;

type Rgba = { w: number; h: number; px: Uint8Array };

// Minimal decoder for what the generator writes: 8-bit RGBA, non-interlaced.
function decodePng(path: string): Rgba {
  const buf = readFileSync(path);
  let off = 8;
  let w = 0;
  let h = 0;
  const idat: Buffer[] = [];
  while (off < buf.length) {
    const len = buf.readUInt32BE(off);
    const type = buf.toString("ascii", off + 4, off + 8);
    const data = buf.subarray(off + 8, off + 8 + len);
    if (type === "IHDR") {
      w = data.readUInt32BE(0);
      h = data.readUInt32BE(4);
      assert.equal(data[8], 8, `${path}: bit depth`);
      assert.equal(data[9], 6, `${path}: colour type must be RGBA`);
      assert.equal(data[12], 0, `${path}: interlaced`);
    } else if (type === "IDAT") {
      idat.push(data);
    }
    off += 12 + len;
  }
  const raw = inflateSync(Buffer.concat(idat));
  const stride = w * 4;
  const px = new Uint8Array(w * h * 4);
  for (let y = 0; y < h; y++) {
    const filter = raw[y * (stride + 1)];
    const line = raw.subarray(y * (stride + 1) + 1, (y + 1) * (stride + 1));
    for (let x = 0; x < stride; x++) {
      const a = x >= 4 ? px[y * stride + x - 4] : 0;
      const b = y > 0 ? px[(y - 1) * stride + x] : 0;
      const c = x >= 4 && y > 0 ? px[(y - 1) * stride + x - 4] : 0;
      let v = line[x];
      if (filter === 1) {
        v += a;
      } else if (filter === 2) {
        v += b;
      } else if (filter === 3) {
        v += (a + b) >> 1;
      } else if (filter === 4) {
        const p = a + b - c;
        const pa = Math.abs(p - a);
        const pb = Math.abs(p - b);
        const pc = Math.abs(p - c);
        v += pa <= pb && pa <= pc ? a : pb <= pc ? b : c;
      }
      px[y * stride + x] = v & 0xff;
    }
  }
  return { w, h, px };
}

function geometry(img: Rgba) {
  const dp = 108 / img.w;
  let top = img.h;
  let bottom = -1;
  let reach = 0;
  for (let y = 0; y < img.h; y++) {
    for (let x = 0; x < img.w; x++) {
      if (img.px[(y * img.w + x) * 4 + 3] > 0) {
        top = Math.min(top, y);
        bottom = Math.max(bottom, y);
        reach = Math.max(reach, Math.hypot(x + 0.5 - img.w / 2, y + 0.5 - img.h / 2));
      }
    }
  }
  return { heightDp: (bottom - top + 1) * dp, reachDp: reach * dp };
}

const fg = decodePng(join(root, adaptive.foregroundImage));
const mono = decodePng(join(root, adaptive.monochromeImage));

test("app.json: adaptive foreground + monochrome on the brand amber", () => {
  assert.equal(adaptive.foregroundImage, "./assets/adaptive-icon.png");
  assert.equal(adaptive.monochromeImage, "./assets/adaptive-monochrome.png");
  assert.equal(adaptive.backgroundColor, "#FABF5A");
  assert.equal(fg.w, fg.h);
  assert.equal(mono.w, fg.w);
});

test("foreground: every painted pixel is inside the 66dp safe zone", () => {
  const { reachDp } = geometry(fg);
  assert.ok(reachDp <= 33.2, `farthest pixel ${reachDp.toFixed(1)}dp from centre (safe radius 33dp)`);
});

test("foreground: the glyph fills the safe zone (not the old 58dp lock)", () => {
  const { heightDp } = geometry(fg);
  assert.ok(heightDp >= 60, `glyph ${heightDp.toFixed(1)}dp tall of 108dp`);
});

test("monochrome: the same shape as the foreground, in white", () => {
  for (let i = 0; i < fg.px.length; i += 4) {
    assert.equal(mono.px[i + 3], fg.px[i + 3], `alpha differs at pixel ${i / 4}`);
    if (mono.px[i + 3] > 0) {
      assert.deepEqual([mono.px[i], mono.px[i + 1], mono.px[i + 2]], [255, 255, 255]);
    }
  }
});
