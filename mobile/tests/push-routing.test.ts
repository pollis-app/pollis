/*
 * How a push payload decides where to route (`mobile/lib/push/routing.ts`).
 *
 * #1122/#1157: the payload used to carry `conversationId`, which meant Expo,
 * APNs and FCM learned which conversation every notification was for — all
 * three sit outside the overlay by design, so that is a disclosure to three
 * third parties on every message, and "which conversation, when" is exactly
 * what the metadata-minimisation design sets out to withhold.
 *
 * The payload now carries ONLY an opaque `h` the client trades for the routing
 * fields over its own authenticated channel. `readPayloadRouting` is the pure
 * half of that, so what counts as routable is testable without a mocked bridge,
 * an emulator, or a real push.
 */

import test from "node:test";
import assert from "node:assert/strict";

import { readPayloadRouting } from "../lib/push/routing.ts";

test("a payload with a handle routes on it", () => {
  assert.deepEqual(readPayloadRouting({ h: "aGFuZGxl" }), {
    handle: "aGFuZGxl",
  });
});

test("a conversation id in the payload is ignored, not used", () => {
  // #1157 removed the field from what the DS sends. Should anything ever put it
  // back — a rolled-back DS, a proxy, a malicious push to a stolen token — the
  // client must not start routing on it again: that would quietly restore the
  // disclosure this whole change exists to remove.
  assert.equal(
    readPayloadRouting({ conversationId: "conv-1", kind: "dm" }),
    null,
  );
});

test("an empty handle is not a handle", () => {
  assert.equal(readPayloadRouting({ h: "" }), null);
});

test("a payload with nothing routable yields null", () => {
  for (const data of [
    null,
    undefined,
    "not-an-object",
    {},
    { h: 7 },
    { h: null },
    { kind: "dm" },
  ]) {
    assert.equal(
      readPayloadRouting(data),
      null,
      `expected null for ${JSON.stringify(data)}`,
    );
  }
});

test("content is never read out of a payload", () => {
  // The push is content-free by construction; this pins that the reader would
  // ignore a body or sender even if a server started sending one.
  assert.deepEqual(
    readPayloadRouting({ h: "aGFuZGxl", body: "hello world", senderId: "alice" }),
    { handle: "aGFuZGxl" },
  );
});
