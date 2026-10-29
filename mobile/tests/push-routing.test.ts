/* How a push payload routes (#1122/#1157): ONLY the opaque `h`, never a conversation id. */

import test from "node:test";
import assert from "node:assert/strict";

import { readPayloadRouting } from "../lib/push/routing.ts";

test("a payload with a handle routes on it", () => {
  assert.deepEqual(readPayloadRouting({ h: "aGFuZGxl" }), {
    handle: "aGFuZGxl",
  });
});

test("a conversation id in the payload is ignored, not used", () => {
  // Never route on it again, whoever puts it back.
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
  // A body or sender would be ignored even if sent.
  assert.deepEqual(
    readPayloadRouting({ h: "aGFuZGxl", body: "hello world", senderId: "alice" }),
    { handle: "aGFuZGxl" },
  );
});
