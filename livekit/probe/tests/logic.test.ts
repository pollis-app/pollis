// node --test livekit/probe/tests/*.test.ts   (or `pnpm test` in livekit/probe/)
import test from "node:test";
import assert from "node:assert/strict";

import {
  DISPATCH_COOLDOWN_MS,
  REMIND_EVERY_MS,
  checkStunResponse,
  decide,
  stunBindingRequest,
  type ProbeResult,
  type ProbeState,
} from "../src/logic.ts";

const MIN = 60_000;

function result(at: number, rtcOk: boolean, turnOk = rtcOk): ProbeResult {
  return {
    at,
    rtc: { ok: rtcOk, detail: rtcOk ? "200 OK" : "502" },
    turn: { ok: turnOk, detail: turnOk ? "STUN binding over TURNS ok" : "TURNS timeout" },
  };
}

test("healthy from nothing: no alert, no dispatch", () => {
  const d = decide(null, result(0, true));
  assert.equal(d.alert, null);
  assert.equal(d.dispatchRestart, false);
  assert.equal(d.state.down, false);
});

test("one failing run is not an outage", () => {
  const d = decide(null, result(0, false));
  assert.equal(d.state.down, false);
  assert.equal(d.state.failures, 1);
  assert.equal(d.alert, null);
  assert.equal(d.dispatchRestart, false);
});

test("second consecutive failure alerts and dispatches once, then respects the cooldown", () => {
  let s: ProbeState | null = null;
  s = decide(s, result(0, false)).state;
  const second = decide(s, result(5 * MIN, false));
  assert.equal(second.state.down, true);
  assert.match(second.alert ?? "", /DOWN/);
  assert.equal(second.dispatchRestart, true);
  assert.equal(second.state.lastDispatchAt, 5 * MIN);

  const third = decide(second.state, result(10 * MIN, false));
  assert.equal(third.dispatchRestart, false);
  assert.equal(third.alert, null);

  const later = decide(third.state, result(5 * MIN + DISPATCH_COOLDOWN_MS, false));
  assert.equal(later.dispatchRestart, true);
});

test("a reminder goes out once an hour while down", () => {
  let s: ProbeState | null = null;
  s = decide(s, result(0, false)).state;
  s = decide(s, result(5 * MIN, false)).state;
  const quiet = decide(s, result(5 * MIN + REMIND_EVERY_MS - MIN, false));
  assert.equal(quiet.alert, null);
  const remind = decide(quiet.state, result(5 * MIN + REMIND_EVERY_MS, false));
  assert.match(remind.alert ?? "", /still down/);
});

test("recovery alerts with the outage length and resets counters", () => {
  let s: ProbeState | null = null;
  s = decide(s, result(0, false)).state;
  s = decide(s, result(5 * MIN, false)).state;
  const up = decide(s, result(20 * MIN, true));
  assert.equal(up.state.down, false);
  assert.equal(up.state.failures, 0);
  assert.equal(up.state.since, null);
  assert.match(up.alert ?? "", /recovered after 20 min/);
  assert.equal(up.dispatchRestart, false);
});

test("either endpoint failing counts as a failure", () => {
  const d = decide(null, result(0, true, false));
  assert.equal(d.state.failures, 1);
});

test("STUN binding request is 20 bytes with the magic cookie", () => {
  const txid = new Uint8Array(12).fill(7);
  const req = stunBindingRequest(txid);
  assert.equal(req.length, 20);
  assert.deepEqual([...req.slice(0, 8)], [0x00, 0x01, 0x00, 0x00, 0x21, 0x12, 0xa4, 0x42]);
  assert.deepEqual([...req.slice(8)], [...txid]);
});

test("STUN response check accepts a success response and rejects everything else", () => {
  const txid = new Uint8Array(12).fill(9);
  const ok = new Uint8Array(20);
  ok.set([0x01, 0x01, 0x00, 0x00, 0x21, 0x12, 0xa4, 0x42]);
  ok.set(txid, 8);
  assert.equal(checkStunResponse(ok, txid).ok, true);

  const errorResp = ok.slice();
  errorResp[1] = 0x11;
  assert.match(checkStunResponse(errorResp, txid).detail, /0x0111/);

  const wrongTx = ok.slice();
  wrongTx[19] = 0;
  assert.equal(checkStunResponse(wrongTx, txid).ok, false);

  const http = new TextEncoder().encode("HTTP/1.1 400 Bad Request\r\n\r\n");
  assert.match(checkStunResponse(http, txid).detail, /not STUN/);

  assert.equal(checkStunResponse(new Uint8Array(3), txid).ok, false);
});
