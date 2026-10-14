/*
 * `errorMessage` (#1305). A Tauri command rejects with the Rust error's text
 * as a plain string, never an `Error`. When this helper only understood
 * `Error`, every string rejection fell through to the caller's fallback — which
 * on the sign-in screen was "Invalid code", shown for a network failure, a 5xx
 * and a keystore error after the code had already been accepted.
 *
 *   node --test frontend/tests/
 */

import test from "node:test";
import assert from "node:assert/strict";

import { errorMessage } from "../src/utils/errorMessage.ts";

test("a string rejection is the message, not the fallback", () => {
  assert.equal(
    errorMessage("ds_post_session /v1/auth/establish-identity 409: already established", "Invalid code"),
    "ds_post_session /v1/auth/establish-identity 409: already established",
  );
});

test("an Error's message wins over the fallback", () => {
  assert.equal(errorMessage(new Error("boom"), "fallback"), "boom");
});

test("nothing usable falls back", () => {
  assert.equal(errorMessage("", "fallback"), "fallback");
  assert.equal(errorMessage("   ", "fallback"), "fallback");
  assert.equal(errorMessage(new Error(""), "fallback"), "fallback");
  assert.equal(errorMessage({ code: 1 }, "fallback"), "fallback");
  assert.equal(errorMessage(undefined), "undefined");
});
