/*
 * Bottom sheets must be opaque (#1193).
 *
 * `SheetOverlay` used to paint its card with `semantic.cardBg`, the accent at
 * 6% alpha. Over a 55% black backdrop that dimmed the channel header and the
 * composer without hiding them, so they showed through the sheet's buttons.
 * Maestro cannot catch this — it reads the view hierarchy, where an element
 * behind a translucent sheet and one behind an opaque sheet look identical —
 * so the invariant is pinned here instead: the sheet surface is an opaque
 * colour for any accent, and every sheet is painted with it.
 *
 * Since the redesign the sheet surface is the derived `raised` tier
 * (theme/derive.ts), which is an opaque mix rather than an alpha composite.
 */
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { compositeOver } from "../theme/composite.ts";
import { deriveTheme } from "../theme/derive.ts";

const BG = "10, 9, 7"; // palette.bg, #0a0907
const ROOT = join(dirname(fileURLToPath(import.meta.url)), "..");
const read = (rel: string) => readFileSync(join(ROOT, rel), "utf8");

test("the composite is opaque and matches the translucent tier over the background", () => {
  // Default amber at 6% over #0a0907: 10*.94+250*.06=24.4, 9*.94+191*.06=19.92,
  // 7*.94+90*.06=11.98.
  assert.equal(compositeOver("250, 191, 90", 0.06, BG), "rgb(24, 20, 12)");
});

test("the composite stays opaque for a custom accent", () => {
  const c = compositeOver("90, 160, 250", 0.06, BG);
  assert.match(c, /^rgb\(\d+, \d+, \d+\)$/, "an rgba() here is a translucent sheet again");
});

test("alpha 0 is the background and alpha 1 is the accent", () => {
  assert.equal(compositeOver("250, 191, 90", 0, BG), "rgb(10, 9, 7)");
  assert.equal(compositeOver("250, 191, 90", 1, BG), "rgb(250, 191, 90)");
});

test("the sheet surface token is an opaque derived tier, never a translucent one", () => {
  const tokens = read("theme/tokens.ts");
  const sheetBg = tokens.match(/get sheetBg\(\)\s*\{\s*return ([^;]+);/);
  assert.ok(sheetBg, "semantic.sheetBg must exist");
  assert.match(
    sheetBg[1],
    /^(_theme\.(raised|panel|high)|tOpaque\()/,
    "sheetBg must be an opaque surface tier (or tOpaque), never t() or an rgba hairline",
  );
});

test("the derived surface tiers are opaque for every accent", () => {
  for (const accent of ["#fabf5a", "#5aa0fa", "#bda3e0"]) {
    const theme = deriveTheme(accent, "#0a0907");
    for (const key of ["panel", "raised", "high"] as const) {
      assert.match(theme[key], /^#[0-9a-f]{6}$/, `${key} for ${accent} must be an opaque hex`);
    }
  }
});

test("SheetOverlay paints with sheetBg, and no sheet repaints itself translucent", () => {
  const overlay = read("components/chat/SheetOverlay.tsx");
  assert.match(overlay, /backgroundColor:\s*semantic\.sheetBg/);
  assert.doesNotMatch(overlay, /semantic\.cardBg/);
  // The three sheets built on SheetOverlay. Inner fields may be tinted (they
  // sit ON the opaque card); the sheet surface itself must not be cardBg.
  for (const sheet of [
    "components/chat/ChannelMenuSheet.tsx",
    "components/chat/MessageActionsSheet.tsx",
    "components/emoji/EmojiPickerSheet.tsx",
  ]) {
    const src = read(sheet);
    assert.match(src, /SheetOverlay/, `${sheet} must be built on SheetOverlay`);
    assert.doesNotMatch(src, /semantic\.cardBg/, `${sheet} must not paint a translucent card`);
  }
});
