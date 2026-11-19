/* #2056: the desktop marks a conversation read only while the user is actually reading it, and
   tells the DS it is in use on change plus at most once a minute on input, so phone pushes are
   held while the user is here and resume once they leave. */
import test from "node:test";
import assert from "node:assert/strict";

import { RENEW_MS, activityReportDue, shouldMarkRead } from "../src/utils/notificationRules.ts";
import { UserActivityStore } from "../src/stores/userActivityStore.ts";

const reading = { active: true, unread: 1, atLiveTail: true, fromBottomPx: 0, thresholdPx: 80 };

test("an open, focused conversation at the bottom is marked read when unread arrives", () => {
  assert.equal(shouldMarkRead(reading), true);
});

test("nothing unread: nothing to mark", () => {
  assert.equal(shouldMarkRead({ ...reading, unread: 0 }), false);
});

test("a blurred, hidden or idle window leaves the conversation unread", () => {
  assert.equal(shouldMarkRead({ ...reading, active: false }), false);
});

test("scrolled up, or viewing an older window of history, does not mark read", () => {
  assert.equal(shouldMarkRead({ ...reading, fromBottomPx: 400 }), false);
  assert.equal(shouldMarkRead({ ...reading, atLiveTail: false }), false);
  assert.equal(shouldMarkRead({ ...reading, fromBottomPx: 80 }), true, "within a row of the bottom counts");
});

test("activity is reported on every change, renewed on input once a minute, never repeated idle", () => {
  assert.equal(activityReportDue(null, true, 0), true, "the first report always goes");
  assert.equal(activityReportDue(null, false, 0), true);
  const active = { active: true, sentAt: 1000 };
  assert.equal(activityReportDue(active, true, 1000 + RENEW_MS - 1), false);
  assert.equal(activityReportDue(active, true, 1000 + RENEW_MS), true);
  assert.equal(activityReportDue(active, false, 1001), true, "going inactive is sent at once");
  const inactive = { active: false, sentAt: 1000 };
  assert.equal(activityReportDue(inactive, false, 1000 + 10 * RENEW_MS), false);
  assert.equal(activityReportDue(inactive, true, 1001), true);
});

test("active means focused, visible and not idle", () => {
  const s = new UserActivityStore();
  assert.equal(s.isActive, true);
  s.setFocused(false);
  assert.equal(s.isActive, false);
  s.setFocused(true);
  s.setVisible(false);
  assert.equal(s.isActive, false);
  s.setVisible(true);
  s.setIdle(true);
  assert.equal(s.isActive, false);
  s.noteInput(Date.now());
  assert.equal(s.isActive, true, "input ends idle");
});
