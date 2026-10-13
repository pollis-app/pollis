/*
 * Touch feedback on a message row (#1282): the timings, the surfaces, and
 * the wiring that keeps it on the UI thread and out of the way of the swipe.
 *
 * The highlight itself is visual and not assertable by Maestro; the flows
 * that drive the row (messaging.yaml's long press, reply.yaml's swipe,
 * double tap, quote tap and long press) prove the gestures still work.
 */
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import {
  LONG_PRESS_MS,
  PRESS_FEEDBACK,
  pressedSurface,
  restingSurface,
} from "../lib/pressFeedback.ts";
import { contrast, deriveTheme } from "../theme/derive.ts";
import { ACCENT_PRESETS, DEFAULT_BACKGROUND_HEX } from "../theme/accents.ts";

const ROOT = join(dirname(fileURLToPath(import.meta.url)), "..");
const read = (p: string) => readFileSync(join(ROOT, p), "utf8");

test("the fade waits out a scroll flick and is fully in before the sheet opens", () => {
  const { delayMs, fadeInMs, fadeOutMs } = PRESS_FEEDBACK;
  // Long enough that a list fling, which takes the touch within a few
  // frames, never flashes the rows under it.
  assert.ok(delayMs >= 80, `delay ${delayMs} ms`);
  // The row is at full highlight before the long press fires.
  assert.ok(delayMs + fadeInMs < LONG_PRESS_MS, `${delayMs} + ${fadeInMs} vs ${LONG_PRESS_MS}`);
  assert.ok(fadeInMs > 0 && fadeOutMs > 0 && fadeOutMs <= 250);
});

test("a pressed row steps one surface up from where it rests", () => {
  assert.equal(restingSurface(false), "bg");
  assert.equal(pressedSurface(false), "raised");
  // A mention or jump-to tint keeps its hue under the finger.
  assert.equal(restingSurface(true), "accentFaint");
  assert.equal(pressedSurface(true), "accentSoft");
});

for (const preset of ACCENT_PRESETS) {
  const theme = deriveTheme(preset.c, DEFAULT_BACKGROUND_HEX);
  test(`${preset.n}: a pressed row keeps AA text and differs from its resting surface`, () => {
    for (const tinted of [false, true]) {
      const pressed = theme[pressedSurface(tinted)];
      const resting = theme[restingSurface(tinted)];
      assert.notEqual(pressed, resting);
      for (const fg of ["text", "dim", "muted", "accent"] as const) {
        const ratio = contrast(theme[fg], pressed);
        assert.ok(ratio >= 4.5, `${fg} on pressed ${pressed} is ${ratio.toFixed(2)}:1`);
      }
    }
  });
}

// ── Wiring ────────────────────────────────────────────────────────────────

const row = read("components/chat/MessageRow.tsx");
const swipe = read("components/chat/SwipeToReply.tsx");

test("the highlight is a Reanimated shared value, not React state", () => {
  assert.match(row, /from "react-native-reanimated"/);
  assert.match(row, /const press = useSharedValue\(0\)/);
  assert.match(row, /interpolateColor\(press\.value/);
  assert.doesNotMatch(row, /useState/);
  assert.match(row, /onPressIn=\{onPressIn\}/);
  assert.match(row, /onPressOut=\{onPressOut\}/);
  assert.match(row, /withDelay\(\s*PRESS_FEEDBACK\.delayMs/);
  assert.match(row, /delayLongPress=\{LONG_PRESS_MS\}/);
  // The resting colour still carries the mention and jump-to tint.
  assert.match(row, /const tinted = mentionsSelf \|\| highlighted;/);
});

test("a swipe clears the highlight on the UI thread", () => {
  assert.match(row, /<SwipeToReply onReply=\{onReply\} pressFeedback=\{press\}>/);
  const onStart = swipe.slice(swipe.indexOf(".onStart("), swipe.indexOf(".onUpdate("));
  assert.match(onStart, /pressFeedback\.value = 0/);
  assert.doesNotMatch(onStart, /scheduleOnRN|runOnJS/);
});

test("attachments open the sheet on the same long press as the row", () => {
  assert.match(read("components/chat/AttachmentTile.tsx"), /delayLongPress: LONG_PRESS_MS/);
});
