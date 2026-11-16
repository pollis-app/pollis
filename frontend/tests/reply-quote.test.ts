/* A quote of a message the viewer never received says why, from the original's ULID. */

import test from "node:test";
import assert from "node:assert/strict";

import { sentBeforeJoin, ulidTimeMs } from "../src/utils/replyQuote.ts";

// 01HQ7Z3K9M… → 2024-02-22T08:32:26.932Z
const ID = "01HQ7Z3K9M2P5R8T1V4W6Y0GEN";
const SENT = Date.parse("2024-02-22T08:32:26.932Z");

test("a ULID's first 10 characters are its send time", () => {
  assert.equal(ulidTimeMs(ID), SENT);
  assert.equal(ulidTimeMs(ID.toLowerCase()), SENT);
  assert.equal(ulidTimeMs("not-a-ulid"), null);
  assert.equal(ulidTimeMs("01HQ7Z3K9U2P5R8T1V4W6Y0GEN"), null);
});

test("before-join only when the original predates the viewer's join", () => {
  assert.equal(sentBeforeJoin(ID, SENT + 1), true);
  assert.equal(sentBeforeJoin(ID, SENT), false);
  assert.equal(sentBeforeJoin(ID, SENT - 1), false);
  // Unknown join time (a DM) or a non-ULID id: never claim it.
  assert.equal(sentBeforeJoin(ID, null), false);
  assert.equal(sentBeforeJoin("legacy-id", SENT + 1), false);
});
