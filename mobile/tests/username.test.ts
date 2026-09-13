/*
 * The mobile copy of the username rule (`lib/username.ts`) — a verbatim twin of
 * `frontend/src/utils/username.ts` (mobile imports no desktop TypeScript), both
 * mirroring what the Delivery Service enforces on `POST /v1/profile/update`.
 *
 * Two things are pinned: that the mirror refuses what the DS refuses (the `@`
 * above all — the DS resolves an identifier with one against emails ONLY, which
 * is sound exactly because no username may contain it), and that the two
 * client copies have not drifted apart.
 *
 *   cd mobile && pnpm test
 */

import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";

import { USERNAME_MAX_LEN, USERNAME_MIN_LEN, USERNAME_PATTERN, isValidUsername } from "../lib/username.ts";

test("an email-shaped username is refused", () => {
  assert.equal(isValidUsername("alice@corp.com"), false);
  assert.equal(isValidUsername("x@y"), false);
});

test("the conforming shapes pass and the rest are refused", () => {
  for (const ok of ["alice", "alice_1234", "a.b-c_d", "abc", "a".repeat(USERNAME_MAX_LEN)]) {
    assert.equal(isValidUsername(ok), true, `${ok} should pass`);
  }
  for (const bad of ["Alice", "ab", "", "has space", "ünïcode", "a".repeat(USERNAME_MAX_LEN + 1)]) {
    assert.equal(isValidUsername(bad), false, `${JSON.stringify(bad)} should be refused`);
  }
  assert.equal(USERNAME_MIN_LEN, 3);
  assert.equal(USERNAME_MAX_LEN, 32);
});

test("the desktop copy declares the same pattern", () => {
  const desktop = readFileSync(
    resolve(import.meta.dirname, "../../frontend/src/utils/username.ts"),
    "utf8",
  );
  const declared = desktop.match(/USERNAME_PATTERN = (\/.*\/);/);
  assert.ok(declared, "desktop must declare USERNAME_PATTERN");
  assert.equal(declared[1], USERNAME_PATTERN.toString());
});
