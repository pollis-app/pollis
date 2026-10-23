/*
 * Which `enrollment_requested` nudges raise the typed-code takeover
 * (`src/utils/enrollmentPrompt.ts`).
 *
 * A phone that scans a desktop's "Link a new device" QR files a link-tagged
 * request; the DS's nudge for it carries `link_id`. The core used to drop that
 * field on its way to the renderer, so the desktop raised "A new device wants
 * to enroll — code shown on the new device" for a phone that shows no code.
 *
 *   node --test frontend/tests/
 */
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
