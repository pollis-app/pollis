/* The sheet entrance can never leave a sheet invisible (#1249, gorhom #2721); Maestro cannot see that. */
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { MOTION } from "../../design/theme.ts";

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
  assert.match(overlay, /settled && !closing \? SETTLED_BACKDROP : null/);
  assert.match(overlay, /settled && !closing \? SETTLED_CARD : null/);
});

test("the settled styles are fully visible", () => {
  assert.match(overlay, /const SETTLED_BACKDROP = \{ opacity: 1 \};/);
  assert.match(overlay, /const SETTLED_CARD = \{ transform: \[\{ translateY: 0 \}\] \};/);
});

/* The close (#2125) plays the entrance backwards, and can never hold a sheet open. */

test("onClose fires from a JS timer at the exit length, never only from the animation", () => {
  assert.match(overlay, /const EXIT_MS = MOTION\.exitMs;/);
  assert.match(overlay, /closeTimer\.current = setTimeout\(finishClose, EXIT_MS\)/);
  assert.match(overlay, /progress\.value = withTiming\(0, \{\s*duration: EXIT_MS,/);
});

test("a close fires onClose once, however many times it is asked for", () => {
  assert.match(overlay, /if \(closingRef\.current \|\| closedRef\.current\) \{\s*return;\s*\}/);
  assert.match(
    overlay,
    /if \(closedRef\.current\) \{\s*return;\s*\}\s*closedRef\.current = true;\s*onCloseRef\.current\(\);/,
  );
});

test("Reduce Motion closes at once", () => {
  assert.match(overlay, /if \(reduceMotion\.current\) \{\s*finishClose\(\);\s*return;\s*\}/);
});

test("every one of the sheet's own dismissals goes through the close", () => {
  assert.match(overlay, /onRequestClose=\{requestClose\}/);
  assert.doesNotMatch(overlay, /onPress=\{onClose\}/);
  assert.equal(overlay.match(/onPress=\{requestClose\}/g)?.length, 2);
});

test("a closing sheet is never snapped back open", () => {
  assert.match(overlay, /if \(!settled \|\| closingRef\.current\) \{/);
});

test("an unmount clears the close timer, so a parent's own close is never doubled", () => {
  assert.match(
    overlay,
    /closedRef\.current = true;\s*if \(closeTimer\.current\) \{\s*clearTimeout\(closeTimer\.current\);/,
  );
});

test("the sheet's motion stays inside the shared ceiling", () => {
  assert.ok(MOTION.exitMs <= MOTION.slowMs);
  assert.ok(MOTION.baseMs <= MOTION.slowMs);
  assert.match(overlay, /const ENTER_MS = MOTION\.baseMs;/);
});
