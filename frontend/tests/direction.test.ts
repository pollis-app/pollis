/* Direction-aware arrow stepping (#902), covering `NavigableGrid`, which `e2e/rtl.spec.ts` can't reach. */

import test from "node:test";
import assert from "node:assert/strict";

import { arrowStep, isHorizontalArrow } from "../src/utils/direction.ts";

test("left-to-right: the key points the way it reads", () => {
  assert.equal(arrowStep("ArrowRight", false), 1);
  assert.equal(arrowStep("ArrowLeft", false), -1);
});

test("right-to-left: next is leftwards", () => {
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
  // Non-horizontal keys return 0, distinct from a step.
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
