// #1360: a new device's approval request must reach the phone by itself. Mobile
// used to drop the DS's `enrollment_requested` nudge at decode, never checked
// on sign-in or resume, and listed the request only on Self → Security — so a
// desktop waiting on "Approve from another device" never showed up on a phone.
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { decodeRealtimeEvent, isTrustedOrigin } from "../lib/realtime/events.ts";
import {
  canPresentApproval,
  isInSignedInApp,
  isTypedCodeRequest,
  nextApproval,
  noticeEnrollmentRequested,
  onEnrollmentRequested,
} from "../lib/enrollmentPrompt.ts";

const read = (p: string) => readFileSync(new URL(p, import.meta.url), "utf8");

test("the DS's enrollment nudge decodes instead of being dropped", () => {
  // The exact shape `pollis-delivery/src/bootstrap.rs` publishes.
  const wire = JSON.stringify({
    type: "enrollment_requested",
    request_id: "01REQ",
    new_device_id: "01DEV",
    verification_code: "ABCD2345",
    link_id: null,
  });
  const event = decodeRealtimeEvent(new TextEncoder().encode(wire));
  assert.ok(event, "enrollment_requested must decode");
  assert.equal(event.type, "enrollment_requested");
  // DS-only: honoured with no publishing participant, dropped with one.
  assert.ok(isTrustedOrigin(event, false));
  assert.ok(!isTrustedOrigin(event, true));
});

test("a QR-link request is not offered for typed-code approval", () => {
  assert.ok(isTypedCodeRequest({ request_id: "r", new_device_id: "d", link_id: null }));
  assert.ok(isTypedCodeRequest({ request_id: "r", new_device_id: "d" }));
  assert.ok(!isTypedCodeRequest({ request_id: "r", new_device_id: "d", link_id: "L" }));
  assert.ok(!isTypedCodeRequest({ request_id: "", new_device_id: "d" }));
});

test("the approval screen only opens inside the signed-in, unlocked app", () => {
  assert.ok(canPresentApproval(["(tabs)", "groups"], false));
  assert.ok(canPresentApproval(["chat", "[id]"], false));
  assert.ok(canPresentApproval(["self", "security"], false));
  // Locked: unlock first.
  assert.ok(!canPresentApproval(["(tabs)", "groups"], true));
  // This device is itself signing in or enrolling.
  assert.ok(!canPresentApproval(["(auth)", "enrollment"], false));
  assert.ok(!canPresentApproval(["(auth)", "pin"], false));
  // Boot redirect, or already showing one.
  assert.ok(!canPresentApproval([], false));
  assert.ok(!canPresentApproval(["approve-device"], false));
});

test("the inbox subscribes only once the device is in the signed-in app", () => {
  // Signed in by the email code but still on the PIN / enrollment screens:
  // the device cannot sign a room-token request yet.
  assert.ok(!isInSignedInApp(["(auth)", "pin"], false));
  assert.ok(!isInSignedInApp(["(auth)", "enrollment"], false));
  assert.ok(isInSignedInApp(["(tabs)", "groups"], false));
  // The approval screen itself is in the app.
  assert.ok(isInSignedInApp(["approve-device"], false));
  const inbox = read("../hooks/useInboxRealtime.ts");
  assert.match(inbox, /isInSignedInApp\(segments, isLocked\)/);
  // A room whose connect failed is re-opened on the way back to the foreground.
  assert.match(inbox, /!isRoomOpen\(room\)/);
});

test("each request opens once, newest first", () => {
  const pending = [
    { request_id: "new", new_device_id: "d2" },
    { request_id: "old", new_device_id: "d1" },
  ];
  assert.equal(nextApproval(pending, new Set())?.request_id, "new");
  assert.equal(nextApproval(pending, new Set(["new"]))?.request_id, "old");
  assert.equal(nextApproval(pending, new Set(["new", "old"])), null);
  assert.equal(nextApproval([], new Set()), null);
});

test("a live nudge reaches the registered handler, a link-bound one does not", () => {
  const seen: string[] = [];
  const off = onEnrollmentRequested((req) => seen.push(req.request_id));
  noticeEnrollmentRequested({ request_id: "a", new_device_id: "d" });
  noticeEnrollmentRequested({ request_id: "b", new_device_id: "d", link_id: "L" });
  off();
  noticeEnrollmentRequested({ request_id: "c", new_device_id: "d" });
  assert.deepEqual(seen, ["a"]);
});

test("the pieces are wired: inbox handler, root gate, route", () => {
  const inbox = read("../hooks/useInboxRealtime.ts");
  assert.match(inbox, /case "enrollment_requested":[\s\S]*?noticeEnrollmentRequested\(event\)/);
  const layout = read("../app/_layout.tsx");
  assert.match(layout, /useEnrollmentApprovals\(\);/);
  assert.match(layout, /name="approve-device"/);
  const hook = read("../hooks/useEnrollmentApprovals.ts");
  // Sign-in/unlock and resume both check; neither is a timer.
  assert.match(hook, /list_pending_enrollment_requests/);
  assert.match(hook, /AppState\.addEventListener/);
  assert.doesNotMatch(hook, /setInterval|refetchInterval/);
});
