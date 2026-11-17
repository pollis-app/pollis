/* #1972: only a whole invite token is previewed; a half-typed one is never sent. */
import test from "node:test";
import assert from "node:assert/strict";

import { completeInviteToken } from "../src/utils/inviteToken.ts";

const TOKEN = `${"A".repeat(16)}.${"b_-".repeat(14)}c`;

test("a bare code, a link and a link with a suffix all reduce to the token", () => {
  assert.equal(completeInviteToken(TOKEN), TOKEN);
  assert.equal(completeInviteToken(`  https://pollis.com/invite/${TOKEN}  `), TOKEN);
  assert.equal(completeInviteToken(`pollis://invite/${TOKEN}?utm=x#frag`), TOKEN);
});

test("anything short of a whole token is not previewed", () => {
  assert.equal(completeInviteToken(""), null);
  assert.equal(completeInviteToken(TOKEN.slice(0, -1)), null);
  assert.equal(completeInviteToken(`${TOKEN}x`), null);
  assert.equal(completeInviteToken(TOKEN.replace(".", "")), null);
  assert.equal(completeInviteToken(`${"A".repeat(16)}.${"b".repeat(42)}!`), null);
});
