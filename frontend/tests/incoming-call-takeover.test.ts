/* #1639: when a ringing DM call takes over the screen, and what the takeover does when the call stops ringing. */

import test from "node:test";
import assert from "node:assert/strict";

import {
  INCOMING_CALL_TAKEOVER_DEFAULT,
  incomingCallPath,
  shouldTakeOver,
  takeoverCallIdFromPath,
  takeoverExit,
  type TakeoverInput,
} from "../src/utils/incomingCallTakeover.ts";

const RINGING = { callId: "c1", roomName: "call-c1" };

const base: TakeoverInput = {
  enabled: true,
  ringing: RINGING,
  pathname: "/dms/conv-1",
  activeVoiceChannelId: null,
  takenOver: new Set(),
};

test("the takeover is on unless turned off", () => {
  assert.equal(INCOMING_CALL_TAKEOVER_DEFAULT, true);
});

test("a ringing call takes over whatever screen the user is on", () => {
  assert.equal(shouldTakeOver(base), true);
  assert.equal(shouldTakeOver({ ...base, pathname: "/groups/g/channels/c" }), true);
  assert.equal(shouldTakeOver({ ...base, pathname: "/terminal" }), true);
  // In a group voice channel: the takeover still offers the call.
  assert.equal(shouldTakeOver({ ...base, activeVoiceChannelId: "voice-1" }), true);
});

test("turned off, nothing takes over", () => {
  assert.equal(shouldTakeOver({ ...base, enabled: false }), false);
});

test("nothing ringing, nothing takes over", () => {
  assert.equal(shouldTakeOver({ ...base, ringing: null }), false);
});

test("a call shown once is never forced back after the user leaves it", () => {
  assert.equal(shouldTakeOver({ ...base, takenOver: new Set(["c1"]) }), false);
  // A different call still takes over.
  assert.equal(
    shouldTakeOver({ ...base, ringing: { callId: "c2", roomName: "call-c2" }, takenOver: new Set(["c1"]) }),
    true,
  );
});

test("no takeover on top of a takeover, the call itself, or a room already joined", () => {
  assert.equal(shouldTakeOver({ ...base, pathname: incomingCallPath("c0") }), false);
  assert.equal(shouldTakeOver({ ...base, pathname: "/call/c1" }), false);
  assert.equal(shouldTakeOver({ ...base, activeVoiceChannelId: "call-c1" }), false);
});

test("takeover paths round-trip, and nothing else parses as one", () => {
  assert.equal(takeoverCallIdFromPath(incomingCallPath("abc")), "abc");
  assert.equal(takeoverCallIdFromPath("/incoming-call/"), null);
  assert.equal(takeoverCallIdFromPath("/incoming-call/abc/x"), null);
  assert.equal(takeoverCallIdFromPath("/call/abc"), null);
  assert.equal(takeoverCallIdFromPath("/"), null);
});

test("the takeover stays while its call rings", () => {
  assert.deepEqual(takeoverExit("c1", "c1", new Set()), { kind: "stay" });
});

test("declined, cancelled, answered elsewhere or stale: back where the user was", () => {
  assert.deepEqual(takeoverExit("c1", null, new Set()), { kind: "back" });
});

test("accepted here: the accept already replaced the screen, so no step back", () => {
  assert.deepEqual(takeoverExit("c1", null, new Set(["c1"])), { kind: "accepted" });
});

test("a different call ringing replaces this one in place", () => {
  assert.deepEqual(takeoverExit("c1", "c2", new Set()), { kind: "switch", callId: "c2" });
});
