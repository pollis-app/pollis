/* #1095: notification text escaping, applied in `bridge/notifications.ts`. */
import test from "node:test";
import assert from "node:assert/strict";

import { escapeNotificationText } from "../src/utils/notificationText.ts";

test("a remote image tag cannot survive into a notification body", () => {
  // The tracking-pixel exploit.
  const groupName = '<img src="http://attacker.test/pixel.png">';
  const escaped = escapeNotificationText(`Invited you to ${groupName}`);
  assert.ok(!escaped.includes("<img"), "the tag must not survive");
  assert.ok(!escaped.includes("<"), "no raw < may remain");
  assert.ok(!escaped.includes(">"), "no raw > may remain");
  assert.ok(escaped.includes("&lt;img"), `expected an escaped tag, got ${escaped}`);
});

test("anchor markup is escaped too", () => {
  const escaped = escapeNotificationText('<a href="http://x">click</a>');
  assert.equal(escaped, '&lt;a href="http://x"&gt;click&lt;/a&gt;');
});

test("ampersands are escaped first so escaping is not double-applied", () => {
  // If `&` were escaped last, `<` would become `&lt;` and then `&amp;lt;`.
  assert.equal(escapeNotificationText("<"), "&lt;");
  assert.equal(escapeNotificationText("&"), "&amp;");
  assert.equal(escapeNotificationText("&lt;"), "&amp;lt;");
});

test("ordinary names are left readable", () => {
  // Display names pass through untouched.
  for (const name of ["Alice", "Bob's laptop", "Café déjà vu", "研究チーム", "a-b_c.d"]) {
    assert.equal(escapeNotificationText(name), name, `${name} must be unchanged`);
  }
});

test("an ampersand in a real group name survives as an entity, not a mangling", () => {
  assert.equal(escapeNotificationText("Alice & Bob"), "Alice &amp; Bob");
});
