/* The sheet entrance can never leave a sheet invisible (#1249, gorhom #2721); Maestro cannot see that. */
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = join(dirname(fileURLToPath(import.meta.url)), "..");
const overlay = readFileSync(join(ROOT, "components/chat/SheetOverlay.tsx"), "utf8");

test("the entrance runs on Reanimated, not the JS-driven Animated API", () => {
  assert.match(overlay, /from "react-native-reanimated"/);
  assert.doesNotMatch(overlay, /Animated\.timing|new Animated\.Value/);
  assert.doesNotMatch(overlay, /useAnimatedReaction/);
});

test("the entrance waits for the Modal to be shown", () => {
  assert.match(overlay, /onShow=\{onShow\}/);
});

test("a timed fallback settles the sheet whatever the animation does", () => {
  assert.match(overlay, /setTimeout\(\(\) => setSettled\(true\), SETTLE_FALLBACK_MS\)/);
  assert.match(overlay, /cancelAnimation\(progress\)/);
  assert.match(overlay, /settled \? SETTLED_BACKDROP : null/);
  assert.match(overlay, /settled \? SETTLED_CARD : null/);
});

test("the settled styles are fully visible", () => {
  assert.match(overlay, /const SETTLED_BACKDROP = \{ opacity: 1 \};/);
  assert.match(overlay, /const SETTLED_CARD = \{ transform: \[\{ translateY: 0 \}\] \};/);
});
