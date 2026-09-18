/*
 * How a push payload decides where to route (`mobile/lib/push/index.ts`).
 *
 * #1122: the payload used to carry `conversationId`, which meant Expo, APNs and
 * FCM learned which conversation every notification was for — all three sit
 * outside the overlay by design, so that is a disclosure to three third parties
 * on every message, and "which conversation, when" is exactly what the
 * metadata-minimisation design sets out to withhold.
 *
 * The payload now carries an opaque `h` the client trades for the routing
 * fields over its own authenticated channel. `readPayloadRouting` is the pure
 * half of that: which of the two a payload is asking for, decided with no I/O,
 * so the precedence and the fallbacks are testable without a mocked bridge, an
 * emulator, or a real push.
 */

import test from "node:test";
import assert from "node:assert/strict";

import { readPayloadRouting } from "../lib/push/routing.ts";

test("a handle is preferred over a plain conversation id", () => {
  // The rollout window sends BOTH. Taking the handle means the plain id stops
  // being used as soon as there is an alternative, rather than only once the
  // DS stops sending it.
  assert.deepEqual(
    readPayloadRouting({ h: "aGFuZGxl", conversationId: "conv-1", kind: "dm" }),
    { via: "handle", handle: "aGFuZGxl" },
  );
});

test("a payload with only a handle routes via the handle", () => {
  assert.deepEqual(readPayloadRouting({ h: "aGFuZGxl" }), {
    via: "handle",
    handle: "aGFuZGxl",
  });
});

test("an older DS's payload still routes on the plain id", () => {
  // A client can be newer than the DS it talks to, so dropping this fallback
  // would break notification taps against any un-upgraded deployment.
  assert.deepEqual(readPayloadRouting({ conversationId: "conv-1", kind: "dm" }), {
    via: "plain",
    conversationId: "conv-1",
    kind: "dm",
  });
});

test("an empty handle is not a handle", () => {
  // An empty string is a present-but-useless field; falling through to the
  // plain id is better than resolving "" and getting nothing.
  assert.deepEqual(
    readPayloadRouting({ h: "", conversationId: "conv-1", kind: "dm" }),
    { via: "plain", conversationId: "conv-1", kind: "dm" },
  );
});

test("a payload with nothing routable yields null", () => {
  for (const data of [
    null,
    undefined,
    "not-an-object",
    {},
    { kind: "dm" },
    { conversationId: "conv-1" },
    { conversationId: 7, kind: "dm" },
    { h: 7 },
  ]) {
    assert.equal(readPayloadRouting(data), null, `expected null for ${JSON.stringify(data)}`);
  }
});

test("content is never read out of a payload", () => {
  // The push is content-free by construction; this pins that the reader would
  // ignore a body/sender even if a server started sending one.
  const decided = readPayloadRouting({
    h: "aGFuZGxl",
    body: "hello world",
    senderId: "alice",
  });
  assert.deepEqual(decided, { via: "handle", handle: "aGFuZGxl" });
});
