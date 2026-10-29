// Screens get the core's error text without uniffi's prefix (#1305).
import test from "node:test";
import assert from "node:assert/strict";

import { plainBridgeError } from "../lib/native/bridgeError.ts";

test("the uniffi prefix is removed", () => {
  const out = plainBridgeError(new Error("BridgeError.Bridge: Invalid code. Please check and try again."));
  assert.ok(out instanceof Error);
  assert.equal((out as Error).message, "Invalid code. Please check and try again.");
});

test("a multi-line core message survives whole", () => {
  const out = plainBridgeError(new Error("BridgeError.Bridge: ds_post_session /v1/x 500:\n{\"error\":\"internal error\"}"));
  assert.equal((out as Error).message, "ds_post_session /v1/x 500:\n{\"error\":\"internal error\"}");
});

test("anything else passes through untouched", () => {
  const plain = new Error("network down");
  assert.equal(plainBridgeError(plain), plain);
  assert.equal(plainBridgeError("str"), "str");
  const empty = new Error("BridgeError.Bridge: ");
  assert.equal(plainBridgeError(empty), empty);
});
