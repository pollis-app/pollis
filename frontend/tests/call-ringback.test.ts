/* #1901: the caller's ringback starts once when a DM call rings and stops on every way the ring ends. */

import test from "node:test";
import assert from "node:assert/strict";

import { Ringback, ringbackWanted, type RingbackSource } from "../src/voice/ringback.ts";

const CALL = { callId: "c1" };
const ROOM = "call-c1";
const ME = { isLocal: true };
const PEER = { isLocal: false };

/** The caller's device while the call rings: in the call room, alone. */
const ringing: RingbackSource = {
  outgoingCall: CALL,
  voiceState: { kind: "joined", channelId: ROOM },
  voiceParticipants: [ME],
};

function harness() {
  const calls: string[] = [];
  const ringback = new Ringback({
    start: () => calls.push("start"),
    stop: () => calls.push("stop"),
  });
  return { calls, ringback };
}

test("a ringing DM call rings back, from the moment the call room is joining", () => {
  assert.equal(ringbackWanted(ringing, true), true);
  assert.equal(
    ringbackWanted({ ...ringing, voiceState: { kind: "joining", channelId: ROOM }, voiceParticipants: [] }, true),
    true,
  );
});

test("no outgoing call (a group or channel call, or nothing): silent", () => {
  assert.equal(ringbackWanted({ ...ringing, outgoingCall: null }, true), false);
});

test("sound effects off: silent", () => {
  assert.equal(ringbackWanted(ringing, false), false);
});

test("answered: the callee joining the room stops it", () => {
  assert.equal(ringbackWanted({ ...ringing, voiceParticipants: [ME, PEER] }, true), false);
});

test("hung up or timed out: leaving the room stops it before the slot clears", () => {
  assert.equal(ringbackWanted({ ...ringing, voiceState: { kind: "leaving", channelId: ROOM } }, true), false);
  assert.equal(ringbackWanted({ ...ringing, voiceState: { kind: "idle" } }, true), false);
});

test("in some other room: silent, whoever is there", () => {
  assert.equal(
    ringbackWanted({ ...ringing, voiceState: { kind: "joined", channelId: "c_lounge" }, voiceParticipants: [ME, PEER] }, true),
    false,
  );
});

test("one start per ring, however often the state is re-read", () => {
  const { calls, ringback } = harness();
  ringback.sync(true);
  ringback.sync(true);
  ringback.sync(true);
  assert.deepEqual(calls, ["start"]);
  assert.equal(ringback.isPlaying, true);
});

test("nothing to stop when it never started", () => {
  const { calls, ringback } = harness();
  ringback.sync(false);
  assert.deepEqual(calls, []);
});

for (const [end, after] of [
  ["answered", { ...ringing, voiceParticipants: [ME, PEER] }],
  ["declined or cancelled (call_canceled clears the outgoing call)", { ...ringing, outgoingCall: null }],
  ["timed out or hung up (the room is left)", { ...ringing, voiceState: { kind: "idle" } }],
] as const) {
  test(`${end}: one start, then exactly one stop`, () => {
    const { calls, ringback } = harness();
    ringback.sync(ringbackWanted(ringing, true));
    ringback.sync(ringbackWanted(after, true));
    ringback.sync(ringbackWanted(after, true));
    assert.deepEqual(calls, ["start", "stop"]);
    assert.equal(ringback.isPlaying, false);
  });
}

test("a second call rings back again", () => {
  const { calls, ringback } = harness();
  ringback.sync(true);
  ringback.sync(false);
  ringback.sync(true);
  assert.deepEqual(calls, ["start", "stop", "start"]);
});
