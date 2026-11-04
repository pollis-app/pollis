/* What the PIN screen shows at each stage (lib/pinStage.ts). */
import test from "node:test";
import assert from "node:assert/strict";
import { pinScreenView, pinSubmitIsSlow } from "../lib/pinStage.ts";

test("unlock and confirm hand off to a slow call; the first entry does not", () => {
  assert.equal(pinSubmitIsSlow("unlock"), true);
  assert.equal(pinSubmitIsSlow("create-confirm"), true);
  assert.equal(pinSubmitIsSlow("create-first"), false);
  assert.equal(pinSubmitIsSlow("checking"), false);
});

test("a submitted PIN swaps the keypad for the loader", () => {
  assert.equal(pinScreenView("unlock", false, false), "keypad");
  assert.equal(pinScreenView("unlock", true, false), "submitting");
  assert.equal(pinScreenView("create-confirm", true, false), "submitting");
  // The first entry only advances to confirm: the pad stays.
  assert.equal(pinScreenView("create-first", true, false), "keypad");
});

test("a wrong PIN puts the keypad back with its error", () => {
  // The lock is released on failure; the error shows on the pad.
  assert.equal(pinScreenView("unlock", false, true), "keypad");
  assert.equal(pinScreenView("create-first", false, true), "keypad");
});

test("reading the unlock state shows the launch loader, but an error there shows the pad (its way out)", () => {
  assert.equal(pinScreenView("checking", false, false), "checking");
  assert.equal(pinScreenView("checking", false, true), "keypad");
});
