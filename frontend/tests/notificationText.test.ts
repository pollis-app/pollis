/*
 * #1095: notification bodies interpolate remote-controlled names (sender
 * username, group name, inviter preferred name) and reach a Linux daemon that
 * renders body markup verbatim. `bridge/notifications.ts` escapes every title
 * and body through this rule at the one point a notification leaves the
 * renderer; these pin the rule itself.
 *
 *   node --test frontend/tests/
 */
import test from "node:test";
import assert from "node:assert/strict";

import { escapeNotificationText } from "../src/utils/notificationText.ts";

test("a remote image tag cannot survive into a notification body", () => {
  // The finding's concrete exploit: a group name that makes the victim's
  // machine fetch a URL the moment a message arrives.
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
  // A display name is not a username: spaces, punctuation and non-Latin
  // scripts are all legitimate and must pass through untouched.
  for (const name of ["Alice", "Bob's laptop", "Café déjà vu", "研究チーム", "a-b_c.d"]) {
    assert.equal(escapeNotificationText(name), name, `${name} must be unchanged`);
  }
});

test("an ampersand in a real group name survives as an entity, not a mangling", () => {
  assert.equal(escapeNotificationText("Alice & Bob"), "Alice &amp; Bob");
});
