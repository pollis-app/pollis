/*
 * The client-side username rule (`src/utils/username.ts`) — a mirror of what
 * the Delivery Service enforces on `POST /v1/profile/update`
 * (`pollis-delivery/src/profile.rs`, `is_valid_username`), so the settings
 * form can explain a refusal before the round trip.
 *
 * The one property that matters for security is the `@`: the DS resolves an
 * identifier that contains one against emails ONLY, which is sound exactly
 * because no username may contain it. `pollis-delivery/tests/username_shape.rs`
 * proves the server and the schema refuse it; this pins that the mirror agrees,
 * so the form never tells the user a name is fine that the DS will reject.
 *
 *   node --test frontend/tests/
 */

import test from "node:test";
import assert from "node:assert/strict";

import { USERNAME_MAX_LEN, USERNAME_MIN_LEN, isValidUsername } from "../src/utils/username.ts";

test("an email-shaped username is refused — the whole finding in one line", () => {
  assert.equal(isValidUsername("alice@corp.com"), false);
  assert.equal(isValidUsername("x@y"), false);
});

test("the conforming shapes pass", () => {
  for (const ok of ["alice", "alice_1234", "a.b-c_d", "abc", "a".repeat(USERNAME_MAX_LEN), "007"]) {
    assert.equal(isValidUsername(ok), true, `${ok} should pass`);
  }
});

test("case, whitespace, unicode and length are refused as the DS refuses them", () => {
  for (const bad of [
    "Alice",
    "ab",
    "",
    "has space",
    " alice",
    "alice\n",
    "ünïcode",
    "with/slash",
    "a".repeat(USERNAME_MAX_LEN + 1),
  ]) {
    assert.equal(isValidUsername(bad), false, `${JSON.stringify(bad)} should be refused`);
  }
  assert.equal(USERNAME_MIN_LEN, 3);
  assert.equal(USERNAME_MAX_LEN, 32);
});
