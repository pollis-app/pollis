/* Bottom sheets must be opaque (#1193); Maestro cannot see translucency. */
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { compositeOver } from "../theme/composite.ts";
import { deriveTheme } from "../theme/derive.ts";

const BG = "10, 9, 7";
const ROOT = join(dirname(fileURLToPath(import.meta.url)), "..");
const read = (rel: string) => readFileSync(join(ROOT, rel), "utf8");

test("the composite is opaque and matches the translucent tier over the background", () => {
  // Default amber at 6% over #0a0907.
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
  // Every sheet built on SheetOverlay must not be cardBg.
  for (const sheet of [
    "components/chat/ChannelMenuSheet.tsx",
    "components/chat/MessageActionsSheet.tsx",
    "components/emoji/EmojiPickerSheet.tsx",
    "components/groups/AddGroupSheet.tsx",
    "components/groups/CreateChannelSheet.tsx",
    "components/groups/GroupMenuSheet.tsx",
  ]) {
    const src = read(sheet);
    assert.match(src, /SheetOverlay/, `${sheet} must be built on SheetOverlay`);
    assert.doesNotMatch(src, /semantic\.cardBg/, `${sheet} must not paint a translucent card`);
  }
});
