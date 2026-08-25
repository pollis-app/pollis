/*
 * Which side the emoji picker opens on, and how tall it is allowed to be.
 *
 * The panel is a fixed `h-[24rem]` anchored to its trigger, and the side was a
 * prop fixed at the call site — `placement="up"` for the composer, because the
 * composer normally sits at the bottom of the window. "Normally" is the bug:
 * when the trigger rides high (a short window, a large font setting, a
 * scrolled layout) the panel renders off the top of the content region, which
 * `AppShell` clips with `overflow: hidden` — so the missing part is not merely
 * off-screen, it is unreachable.
 *
 * The geometry needs a browser; the decision made from it does not, and the
 * decision is where the rule lives.
 *
 *   node --test frontend/tests/
 */

import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { resolve, dirname } from "node:path";
import { fileURLToPath } from "node:url";

import {
  MIN_PANEL_HEIGHT_PX,
  PANEL_GAP_PX,
  resolvePlacement,
} from "../src/components/Emoji/pickerPlacement.ts";

/** 24rem at a 16px root. */
const DESIRED = 384;

test("the preference is honoured whenever the panel fits there", () => {
  assert.deepEqual(
    resolvePlacement({ preferred: "up", spaceAbove: 600, spaceBelow: 40, desired: DESIRED }),
    { placement: "up" },
  );
  assert.deepEqual(
    resolvePlacement({ preferred: "down", spaceAbove: 40, spaceBelow: 600, desired: DESIRED }),
    { placement: "down" },
  );
});

test("the panel flips when the preferred side cannot seat it", () => {
  assert.equal(
    resolvePlacement({ preferred: "up", spaceAbove: 120, spaceBelow: 600, desired: DESIRED })
      .placement,
    "down",
  );
  assert.equal(
    resolvePlacement({ preferred: "down", spaceAbove: 600, spaceBelow: 120, desired: DESIRED })
      .placement,
    "up",
  );
});

test("the gap counts against the space, so a panel that fits by a hair still flips", () => {
  assert.equal(
    resolvePlacement({
      preferred: "up",
      spaceAbove: DESIRED + PANEL_GAP_PX - 1,
      spaceBelow: 900,
      desired: DESIRED,
    }).placement,
    "down",
  );
  assert.equal(
    resolvePlacement({
      preferred: "up",
      spaceAbove: DESIRED + PANEL_GAP_PX,
      spaceBelow: 900,
      desired: DESIRED,
    }).placement,
    "up",
  );
});

test("neither side fitting clamps the panel to the roomier one", () => {
  const r = resolvePlacement({
    preferred: "up",
    spaceAbove: 100,
    spaceBelow: 300,
    desired: DESIRED,
  });
  assert.equal(r.placement, "down");
  assert.equal(r.maxHeight, 300 - PANEL_GAP_PX);
});

test("a tie keeps the preference rather than flipping about", () => {
  assert.equal(
    resolvePlacement({ preferred: "up", spaceAbove: 200, spaceBelow: 200, desired: DESIRED })
      .placement,
    "up",
  );
});

test("the clamp never shrinks the panel below a usable one", () => {
  assert.equal(
    resolvePlacement({ preferred: "up", spaceAbove: 10, spaceBelow: 20, desired: DESIRED })
      .maxHeight,
    MIN_PANEL_HEIGHT_PX,
  );
});

test("a panel that fits is given no height — its own class keeps governing", () => {
  assert.equal(
    resolvePlacement({ preferred: "up", spaceAbove: 900, spaceBelow: 900, desired: DESIRED })
      .maxHeight,
    undefined,
  );
});

// A larger font setting scales the panel (it is sized in rem) while the
// viewport does not — which is exactly when it stops fitting.
test("the same space flips for a bigger panel", () => {
  const space = { preferred: "up" as const, spaceAbove: 400, spaceBelow: 900 };
  assert.equal(resolvePlacement({ ...space, desired: 384 }).placement, "up");
  assert.equal(resolvePlacement({ ...space, desired: 480 }).placement, "down");
});

// `PANEL_REM_HEIGHT` is a hand-copy of a Tailwind class in another file. If the
// panel is resized and the copy is not, placement is resolved against a height
// the panel no longer has — and the failure is a clipped picker, which is the
// bug this whole module exists to stop.
test("the height the button resolves against is the height the picker renders", () => {
  const root = resolve(dirname(fileURLToPath(import.meta.url)), "../src/components/Emoji");
  const rendered = readFileSync(resolve(root, "EmojiPicker.tsx"), "utf8").match(
    /h-\[(\d+(?:\.\d+)?)rem\]/,
  );
  const assumed = readFileSync(resolve(root, "EmojiPickerButton.tsx"), "utf8").match(
    /PANEL_REM_HEIGHT\s*=\s*(\d+(?:\.\d+)?)/,
  );

  assert.ok(rendered, "EmojiPicker no longer sets its height in rem");
  assert.ok(assumed, "EmojiPickerButton no longer declares PANEL_REM_HEIGHT");
  assert.equal(
    Number(assumed[1]),
    Number(rendered[1]),
    "PANEL_REM_HEIGHT has drifted from EmojiPicker's h-[..rem]",
  );
});
