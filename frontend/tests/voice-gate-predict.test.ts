/* Optimistic gate prediction (#849): the renderer's same-frame guess must match what `TransmitGate` replies. */

import test from "node:test";
import assert from "node:assert/strict";

import { predictGate, COALESCED_GATE_COMMANDS } from "../src/voice/gatePredict.ts";
import type { VoiceGateState } from "../src/types/voice-state.ts";

const LIVE: VoiceGateState = {
  mode: "voice_activity",
  self_muted: false,
  deafened: false,
  ptt_held: false,
  transmitting: true,
};

test("mute closes the mic and unmute reopens it", () => {
  const muted = predictGate(LIVE, "toggle_voice_mute", undefined, null);
  assert.deepEqual(muted, { ...LIVE, self_muted: true, transmitting: false });
  assert.deepEqual(predictGate(muted!, "toggle_voice_mute", undefined, null), LIVE);
});

test("deafen forces mute; undeafen restores the mute that was in force before", () => {
  const deafened = predictGate(LIVE, "toggle_voice_deafen", undefined, null);
  assert.deepEqual(deafened, { ...LIVE, deafened: true, self_muted: true, transmitting: false });
  // Was live before the deafen → undeafen opens the mic again.
  assert.deepEqual(predictGate(deafened!, "toggle_voice_deafen", undefined, false), LIVE);
  // Was muted before the deafen → the mute survives the undeafen.
  assert.deepEqual(predictGate(deafened!, "toggle_voice_deafen", undefined, true), {
    ...LIVE,
    self_muted: true,
    transmitting: false,
  });
});

test("unmuting while deafened also undeafens (deafened ⇒ self_muted holds)", () => {
  const deafened: VoiceGateState = { ...LIVE, deafened: true, self_muted: true, transmitting: false };
  const next = predictGate(deafened, "toggle_voice_mute", undefined, null);
  assert.deepEqual(next, LIVE);
});

test("push-to-talk: idle is not transmitting, held is, and a mode switch drops the key", () => {
  const ptt = predictGate(LIVE, "set_voice_input_mode", { mode: "push_to_talk" }, null);
  assert.deepEqual(ptt, { ...LIVE, mode: "push_to_talk", transmitting: false });
  const held = predictGate(ptt!, "set_voice_ptt_held", { held: true }, null);
  assert.equal(held!.transmitting, true);
  assert.equal(predictGate(held!, "release_voice_ptt", undefined, null)!.transmitting, false);
  // Held while muted stays closed.
  const mutedPtt = { ...ptt!, self_muted: true };
  assert.equal(predictGate(mutedPtt, "set_voice_ptt_held", { held: true }, null)!.transmitting, false);
  // Switching back to voice activity releases a held key.
  const back = predictGate(held!, "set_voice_input_mode", { mode: "voice_activity" }, null);
  assert.deepEqual(back, LIVE);
});

test("a read-only sync and an unknown mode predict nothing", () => {
  assert.equal(predictGate(LIVE, "get_voice_gate_state", undefined, null), null);
  assert.equal(predictGate(LIVE, "set_voice_input_mode", { mode: "nonsense" }, null), null);
});

test("only the two toggles coalesce; PTT key events never drop", () => {
  assert.ok(COALESCED_GATE_COMMANDS.has("toggle_voice_mute"));
  assert.ok(COALESCED_GATE_COMMANDS.has("toggle_voice_deafen"));
  assert.ok(!COALESCED_GATE_COMMANDS.has("set_voice_ptt_held"));
  assert.ok(!COALESCED_GATE_COMMANDS.has("release_voice_ptt"));
});
