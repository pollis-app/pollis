/*
 * The renderer matches typed voice-join errors on their kind, never on English.
 */

import test from "node:test";
import assert from "node:assert/strict";

import {
  joinErrorMessageKey,
  legacyJoinErrorKey,
  parseVoiceJoinError,
} from "../src/voice/joinError.ts";

test("the wire form parses into kind and detail", () => {
  assert.deepEqual(parseVoiceJoinError("voice_join:not_member: ds_post /v1/livekit/token 403"), {
    kind: "not_member",
    detail: "ds_post /v1/livekit/token 403",
  });
  assert.deepEqual(parseVoiceJoinError("voice_join:cancelled"), { kind: "cancelled", detail: null });
  assert.deepEqual(parseVoiceJoinError("voice_join:deadline: connect"), { kind: "deadline", detail: "connect" });
});

test("plain strings are not typed errors", () => {
  assert.equal(parseVoiceJoinError("LiveKit connect: signal failure: timed out"), null);
  assert.equal(parseVoiceJoinError("voice_join:"), null);
  assert.equal(parseVoiceJoinError("voice_join:Not Member"), null);
});

test("every kind maps to a catalogue key except a user cancel, which shows nothing", () => {
  assert.equal(joinErrorMessageKey("cancelled"), null);
  assert.equal(joinErrorMessageKey("ds_unreachable"), "voice:joinError.unreachable");
  assert.equal(joinErrorMessageKey("livekit_unreachable"), "voice:joinError.unreachable");
  assert.equal(joinErrorMessageKey("token_rejected"), "voice:joinError.tokenRejected");
  assert.equal(joinErrorMessageKey("not_member"), "voice:joinError.notMember");
  assert.equal(joinErrorMessageKey("no_mls_group"), "voice:joinError.noMlsGroup");
  assert.equal(joinErrorMessageKey("deadline"), "voice:joinError.deadline");
  assert.equal(joinErrorMessageKey("publish_failed"), "voice:joinError.publishFailed");
  assert.equal(joinErrorMessageKey("join_in_progress"), "voice:joinError.busy");
  assert.equal(joinErrorMessageKey("internal"), "voice:joinError.generic");
  // A kind this build does not know yet still shows something, not a raw string.
  assert.equal(joinErrorMessageKey("some_future_kind"), "voice:joinError.generic");
});

test("an older core's English still collapses to the unreachable line", () => {
  assert.equal(legacyJoinErrorKey("LiveKit connect: signal failure: ws error"), "voice:joinError.unreachable");
  assert.equal(legacyJoinErrorKey("LiveKit is not configured on this server"), "voice:joinError.notConfigured");
  assert.equal(legacyJoinErrorKey("publish track: boom"), null);
});
