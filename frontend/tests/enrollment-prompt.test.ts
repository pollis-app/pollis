/* Which nudges raise the typed-code takeover; a QR-linked phone request (`link_id`) shows no code. */
import test from "node:test";
import assert from "node:assert/strict";

import { isTypedCodeRequest } from "../src/utils/enrollmentPrompt.ts";

test("a request without a link id raises the typed-code takeover", () => {
  assert.equal(isTypedCodeRequest({ request_id: "r1", new_device_id: "d2" }), true);
  assert.equal(isTypedCodeRequest({ request_id: "r1", new_device_id: "d2", link_id: null }), true);
  assert.equal(isTypedCodeRequest({ request_id: "r1", new_device_id: "d2", link_id: "" }), true);
});

test("a QR-link request never raises the typed-code takeover", () => {
  assert.equal(isTypedCodeRequest({ request_id: "r1", new_device_id: "d2", link_id: "link-1" }), false);
});

test("a nudge missing its ids raises nothing", () => {
  assert.equal(isTypedCodeRequest({ new_device_id: "d2" }), false);
  assert.equal(isTypedCodeRequest({ request_id: "r1" }), false);
});
