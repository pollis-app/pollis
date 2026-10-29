/*
 * Pointer-down activation where an accidental press is free to undo (#1529).
 *
 * `utils/pressActivation.ts` decides, per event, whether a press acts on
 * pointerdown or waits for the click. These pin the contract the hook leans
 * on: only a plain primary mouse press acts early, the click that ends that
 * press never runs the action a second time, and the keyboard / assistive
 * tech path (a click with `detail === 0`) always acts.
 *
 *   node --test frontend/tests/
 */

import test from "node:test";
import assert from "node:assert/strict";

import { createPressGuard, isPlainPrimaryMousePress, type PressLike } from "../src/utils/pressActivation.ts";

const mouse = (over: Partial<PressLike> = {}): PressLike => ({
  pointerType: "mouse",
  button: 0,
  ctrlKey: false,
  metaKey: false,
  shiftKey: false,
  altKey: false,
  ...over,
});

test("only a plain primary mouse press counts", () => {
  assert.equal(isPlainPrimaryMousePress(mouse()), true);
  assert.equal(isPlainPrimaryMousePress(mouse({ pointerType: "touch" })), false);
  assert.equal(isPlainPrimaryMousePress(mouse({ pointerType: "pen" })), false);
  assert.equal(isPlainPrimaryMousePress(mouse({ button: 1 })), false);
  assert.equal(isPlainPrimaryMousePress(mouse({ button: 2 })), false);
  for (const key of ["ctrlKey", "metaKey", "shiftKey", "altKey"] as const) {
    assert.equal(isPlainPrimaryMousePress(mouse({ [key]: true })), false, key);
  }
});

test("a mouse press acts on pointerdown and its click is swallowed", () => {
  const g = createPressGuard();
  let runs = 0;
  const action = () => runs++;
  assert.equal(g.pointerDown(mouse(), action), true);
  assert.equal(runs, 1);
  g.click({ detail: 1 }, action);
  assert.equal(runs, 1, "exactly once per press");
  // The next press acts again.
  g.pointerDown(mouse(), action);
  g.click({ detail: 1 }, action);
  assert.equal(runs, 2);
});

test("Enter / Space / assistive tech (detail 0) always act on click", () => {
  const g = createPressGuard();
  let runs = 0;
  const action = () => runs++;
  g.click({ detail: 0 }, action);
  g.click({ detail: 0 }, action);
  assert.equal(runs, 2);
  // Even right after a press whose release landed elsewhere.
  g.pointerDown(mouse(), action);
  g.click({ detail: 0 }, action);
  assert.equal(runs, 4);
});

test("touch, pen and modified clicks wait for the click", () => {
  const g = createPressGuard();
  let runs = 0;
  const action = () => runs++;
  for (const e of [mouse({ pointerType: "touch" }), mouse({ pointerType: "pen" }), mouse({ metaKey: true })]) {
    assert.equal(g.pointerDown(e, action), false);
    assert.equal(runs, 0);
  }
  g.click({ detail: 1 }, action);
  assert.equal(runs, 1);
});

test("a non-plain press clears a stale arm", () => {
  const g = createPressGuard();
  let runs = 0;
  const action = () => runs++;
  // Mouse press dragged off: no click arrives.
  g.pointerDown(mouse(), action);
  assert.equal(g.armed, true);
  // A touch tap afterwards must still act on its click.
  g.pointerDown(mouse({ pointerType: "touch" }), action);
  g.click({ detail: 1 }, action);
  assert.equal(runs, 2);
});

test("disarm forgets a press whose release landed elsewhere", () => {
  const g = createPressGuard();
  let runs = 0;
  const action = () => runs++;
  g.pointerDown(mouse(), action);
  g.disarm();
  g.click({ detail: 1 }, action);
  assert.equal(runs, 2);
});
