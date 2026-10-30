/* The Android notification small icon is the lock glyph alone, filling the 24dp status-bar box (#1606). */

import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { decodePng } from "./png.ts";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const app = JSON.parse(readFileSync(join(root, "app.json"), "utf8")).expo;
const plugin = app.plugins.find((p: unknown) => Array.isArray(p) && p[0] === "expo-notifications");
const options = plugin?.[1] ?? {};

test("app.json: expo-notifications has a dedicated small icon and keeps the accent tint", () => {
  assert.equal(options.icon, "./assets/notification-icon.png");
  assert.equal(options.color, "#e6b65a");
});

test("icon: xxxhdpi, white on transparency (Android uses only its alpha)", () => {
  const img = decodePng(join(root, options.icon));
  // 96px = 24dp at xxxhdpi; Expo derives the lower densities from it.
  assert.equal(img.w, 96);
  assert.equal(img.h, 96);
  for (let i = 0; i < img.px.length; i += 4) {
    if (img.px[i + 3] > 0) {
      assert.deepEqual([img.px[i], img.px[i + 1], img.px[i + 2]], [255, 255, 255], `pixel ${i / 4} is not white`);
    }
  }
});

test("icon: the glyph fills the 24dp box, not the launcher icon's padded lock", () => {
  const img = decodePng(join(root, options.icon));
  const dp = 24 / img.w;
  let top = img.h;
  let bottom = -1;
  let left = img.w;
  let right = -1;
  for (let y = 0; y < img.h; y++) {
    for (let x = 0; x < img.w; x++) {
      if (img.px[(y * img.w + x) * 4 + 3] > 0) {
        top = Math.min(top, y);
        bottom = Math.max(bottom, y);
        left = Math.min(left, x);
        right = Math.max(right, x);
      }
    }
  }
  const heightDp = (bottom - top + 1) * dp;
  assert.ok(heightDp >= 21.5, `glyph ${heightDp.toFixed(1)}dp tall of 24dp`);
  // At least a hairline of margin on every side, so nothing touches the box edge.
  assert.ok(top * dp >= 0.5 && (img.h - 1 - bottom) * dp >= 0.5, "glyph touches the top or bottom edge");
  assert.ok(left * dp >= 0.5 && (img.w - 1 - right) * dp >= 0.5, "glyph touches a side edge");
});
