/* A `type.*` step is a new object after every palette or script change, so React Native repaints it (#2107). */
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { snapshotSteps } from "../theme/styleSnapshot.ts";

const ROOT = join(dirname(fileURLToPath(import.meta.url)), "..");

function liveSteps() {
  const state = { text: "#f5f1ea", generation: 0 };
  const live = {
    title: {
      fontFamily: "Geist_700Bold",
      fontSize: 20,
      get color() {
        return state.text;
      },
    },
    button: { fontFamily: "Geist_600SemiBold", fontSize: 15 },
  };
  return { state, steps: snapshotSteps(live, () => state.generation) };
}

test("a step is the same object while the palette holds", () => {
  const { steps } = liveSteps();
  assert.equal(steps.title, steps.title);
  assert.equal(steps.button, steps.button);
});

test("a palette change gives every step a new object with the new colour", () => {
  const { state, steps } = liveSteps();
  const before = steps.title;
  const button = steps.button;
  assert.equal(before.color, "#f5f1ea");
  // A light background: dark text.
  state.text = "#1a1714";
  state.generation++;
  const after = steps.title;
  // React Native skips a style prop whose object is unchanged, so a new object is what repaints it.
  assert.notEqual(after, before);
  assert.equal(after.color, "#1a1714");
  assert.notEqual(steps.button, button);
});

test("a snapshot is plain data, so a spread or a style array reads the same values", () => {
  const { steps } = liveSteps();
  const desc = Object.getOwnPropertyDescriptor(steps.title, "color");
  assert.equal(desc?.get, undefined);
  assert.equal(desc?.value, "#f5f1ea");
  assert.deepEqual({ ...steps.title }, { fontFamily: "Geist_700Bold", fontSize: 20, color: "#f5f1ea" });
  assert.deepEqual(Object.keys(steps), ["title", "button"]);
});

test("tokens.ts renews the snapshots on every accent, background and script change", () => {
  const src = readFileSync(join(ROOT, "theme/tokens.ts"), "utf8");
  for (const fn of ["setAccentRgb", "setBackgroundRgb", "setScriptMetrics"]) {
    const body = src.match(new RegExp(`export function ${fn}\\([^)]*\\)[^{]*\\{([\\s\\S]*?)\\n\\}`));
    assert.ok(body, `${fn} not found`);
    assert.match(body![1], /_styleGeneration\+\+/, `${fn} must bump _styleGeneration`);
  }
  assert.match(src, /export const type = snapshotSteps\(liveType, \(\) => _styleGeneration\);/);
});

test("the theme key changes with the background, not only the accent", () => {
  const src = readFileSync(join(ROOT, "components/theme.tsx"), "utf8");
  assert.match(src, /themeKey: `\$\{accentHex\}\|\$\{backgroundHex\}`/);
});
