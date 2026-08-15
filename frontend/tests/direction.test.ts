/*
 * Direction-aware arrow-key stepping (#902).
 *
 * Runs on Node's built-in test runner with native TypeScript type stripping —
 * no test framework dependency, and deliberately OUTSIDE `src/` so the
 * production `tsc --noEmit` does not try to typecheck a file importing
 * `node:test`.
 *
 * `e2e/rtl.spec.ts` proves the two consumers a browser can reach
 * (`NavigableList` via the shortcuts page, `EmojiPicker` via the composer) move
 * focus the right way by MEASURING where it landed. The third consumer,
 * `NavigableGrid`, only ever renders inside the voice stage, which needs a
 * live LiveKit session the browser tier does not have — so its half of the fix
 * is covered here, at the shared helper all three call.
 *
 *   node --test frontend/tests/
 */

import test from "node:test";
import assert from "node:assert/strict";

import { arrowStep, isHorizontalArrow } from "../src/utils/direction.ts";

test("left-to-right: the key points the way it reads", () => {
  assert.equal(arrowStep("ArrowRight", false), 1);
  assert.equal(arrowStep("ArrowLeft", false), -1);
});

test("right-to-left: next is leftwards", () => {
  // The whole bug in one assertion. `ArrowRight` used to be a hardcoded +1,
  // which walked an Arabic user backwards through their own list.
  assert.equal(arrowStep("ArrowRight", true), -1);
  assert.equal(arrowStep("ArrowLeft", true), 1);
});

test("the two keys are inverses of each other in both directions", () => {
  for (const rtl of [false, true]) {
    assert.equal(
      arrowStep("ArrowRight", rtl) + arrowStep("ArrowLeft", rtl),
      0,
      `keys did not cancel with rtl=${rtl}`,
    );
  }
});

test("vertical arrows are not horizontal and do not mirror", () => {
  // `dir` mirrors the INLINE axis only; the block axis is top-to-bottom in
  // every locale. A caller must be able to tell "not my key" from "step zero",
  // which is what the 0 return is for.
  for (const key of ["ArrowUp", "ArrowDown", "Home", "End", "Enter", "a"]) {
    assert.equal(isHorizontalArrow(key), false, `${key} claimed to be horizontal`);
    assert.equal(arrowStep(key, false), 0);
    assert.equal(arrowStep(key, true), 0);
  }
});

test("horizontal arrows are recognized as such", () => {
  assert.equal(isHorizontalArrow("ArrowRight"), true);
  assert.equal(isHorizontalArrow("ArrowLeft"), true);
});
