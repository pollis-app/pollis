/* Which side the emoji picker opens on, and its max height; a fixed side clipped it unreachably under AppShell. */

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

// rem-sized panel, px viewport: a larger font stops it fitting.
test("the same space flips for a bigger panel", () => {
  const space = { preferred: "up" as const, spaceAbove: 400, spaceBelow: 900 };
  assert.equal(resolvePlacement({ ...space, desired: 384 }).placement, "up");
  assert.equal(resolvePlacement({ ...space, desired: 480 }).placement, "down");
});

// `PANEL_REM_HEIGHT` hand-copies a Tailwind class; they must agree.
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
