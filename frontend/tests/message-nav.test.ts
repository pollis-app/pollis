/*
 * Arrow-key chat-log navigation — the pure state machine.
 *
 * The DOM half (focus projection, key translation) lives in `MessageList` and
 * needs a browser; every focus-position invariant lives HERE, in
 * `transitionMessageNav`, which is exactly why it is a pure function. These
 * tests pin the whole contract: entry lands on the newest message, the ends
 * clamp, walking rows drops any action selection, the action bar is entered
 * from its start and left past its start, vanished messages re-anchor, and an
 * empty log refuses entry.
 *
 *   node --test frontend/tests/
 */

import test from "node:test";
import assert from "node:assert/strict";

import {
  transitionMessageNav,
  type MessageNavAction,
  type MessageNavState,
} from "../src/utils/messageNav.ts";

const IDS = ["m1", "m2", "m3"];
// m2 is the viewer's own message (has edit); m3 is a bare tombstone.
const ACTIONS: Record<string, MessageNavAction[]> = {
  m1: ["reply", "more"],
  m2: ["reply", "edit", "more"],
  m3: [],
};
const actionsFor = (id: string) => ACTIONS[id] ?? [];

const step = (state: MessageNavState, type: Parameters<typeof transitionMessageNav>[1]["type"]) =>
  transitionMessageNav(state, { type }, IDS, actionsFor);

test("enter focuses the newest message", () => {
  assert.deepEqual(step({ mode: "composer" }, "enter"), {
    mode: "message",
    messageId: "m3",
  });
});

test("enter on an empty log stays in the composer", () => {
  const state: MessageNavState = { mode: "composer" };
  assert.equal(transitionMessageNav(state, { type: "enter" }, [], actionsFor), state);
});

test("up walks older and clamps at the oldest", () => {
  const atNewest: MessageNavState = { mode: "message", messageId: "m3" };
  const older = step(atNewest, "up");
  assert.deepEqual(older, { mode: "message", messageId: "m2" });
  const oldest = step(older, "up");
  assert.deepEqual(oldest, { mode: "message", messageId: "m1" });
  // Clamp: same object back, so effects keyed on identity don't refire.
  assert.equal(step(oldest, "up"), oldest);
});

test("down walks newer and exits to the composer past the newest", () => {
  const oldest: MessageNavState = { mode: "message", messageId: "m1" };
  assert.deepEqual(step(oldest, "down"), { mode: "message", messageId: "m2" });
  assert.deepEqual(step({ mode: "message", messageId: "m3" }, "down"), {
    mode: "composer",
  });
});

test("movement keys are inert in the composer", () => {
  const state: MessageNavState = { mode: "composer" };
  for (const type of ["up", "down", "nextAction", "prevAction"] as const) {
    assert.equal(step(state, type), state);
  }
});

test("nextAction enters the bar at its start and clamps at its end", () => {
  const row: MessageNavState = { mode: "message", messageId: "m2" };
  const first = step(row, "nextAction");
  assert.deepEqual(first, { mode: "action", messageId: "m2", action: "reply" });
  const second = step(first, "nextAction");
  assert.deepEqual(second, { mode: "action", messageId: "m2", action: "edit" });
  const third = step(second, "nextAction");
  assert.deepEqual(third, { mode: "action", messageId: "m2", action: "more" });
  assert.equal(step(third, "nextAction"), third);
});

test("prevAction walks back and returns to the bare row from the first action", () => {
  const atEdit: MessageNavState = { mode: "action", messageId: "m2", action: "edit" };
  assert.deepEqual(step(atEdit, "prevAction"), {
    mode: "action",
    messageId: "m2",
    action: "reply",
  });
  assert.deepEqual(
    step({ mode: "action", messageId: "m2", action: "reply" }, "prevAction"),
    { mode: "message", messageId: "m2" },
  );
});

test("a row with no actions (tombstone) cannot enter action mode", () => {
  const row: MessageNavState = { mode: "message", messageId: "m3" };
  assert.equal(step(row, "nextAction"), row);
});

test("row movement drops any action selection", () => {
  const atAction: MessageNavState = { mode: "action", messageId: "m2", action: "edit" };
  assert.deepEqual(step(atAction, "up"), { mode: "message", messageId: "m1" });
  assert.deepEqual(step(atAction, "down"), { mode: "message", messageId: "m3" });
});

test("an action that vanished from the row clamps to the bar's start", () => {
  // The viewer's message was re-attributed / edit got disabled mid-selection.
  const stale: MessageNavState = { mode: "action", messageId: "m1", action: "edit" };
  assert.deepEqual(step(stale, "nextAction"), {
    mode: "action",
    messageId: "m1",
    action: "reply",
  });
});

test("a vanished message re-anchors to the newest", () => {
  const gone: MessageNavState = { mode: "message", messageId: "deleted" };
  assert.deepEqual(step(gone, "up"), { mode: "message", messageId: "m3" });
});

test("a vanished message on an emptied log falls back to the composer", () => {
  const gone: MessageNavState = { mode: "message", messageId: "deleted" };
  assert.deepEqual(transitionMessageNav(gone, { type: "up" }, [], actionsFor), {
    mode: "composer",
  });
});

test("exit and cancel both land in the composer", () => {
  const atAction: MessageNavState = { mode: "action", messageId: "m2", action: "more" };
  assert.deepEqual(step(atAction, "exit"), { mode: "composer" });
  assert.deepEqual(step(atAction, "cancel"), { mode: "composer" });
});
