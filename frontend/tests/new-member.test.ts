/*
 * The "new" member tag (#1506): a member's messages are tagged for seven days
 * after their server-side `joined_at`, in every channel of the group, and the
 * group's creator is never tagged.
 *
 *   node --test frontend/tests/
 */

import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

import {
  NEW_MEMBER_WINDOW_MS,
  isNewMemberMessage,
  newMemberJoins,
  parseServerTimestamp,
} from "../src/utils/newMember.ts";

const JOINED = "2026-10-01 12:00:00";
const JOINED_MS = Date.UTC(2026, 9, 1, 12, 0, 0);

test("a zone-less server timestamp is UTC, whatever the viewer's zone", () => {
  assert.equal(parseServerTimestamp(JOINED), JOINED_MS);
  assert.equal(parseServerTimestamp("2026-10-01T12:00:00Z"), JOINED_MS);
  assert.equal(parseServerTimestamp("2026-10-01T14:00:00+02:00"), JOINED_MS);
  assert.equal(parseServerTimestamp(""), null);
  assert.equal(parseServerTimestamp("not a date"), null);
});

test("tagged from joining until seven days after, and never after", () => {
  const joins = newMemberJoins([{ user_id: "u1", joined_at: JOINED }], "owner");
  assert.equal(isNewMemberMessage(joins, "u1", JOINED_MS), true);
  assert.equal(isNewMemberMessage(joins, "u1", JOINED_MS + NEW_MEMBER_WINDOW_MS - 1), true);
  assert.equal(isNewMemberMessage(joins, "u1", JOINED_MS + NEW_MEMBER_WINDOW_MS), false);
});

test("a sender whose clock is a little behind is still tagged; a rejoiner's old messages are not", () => {
  const joins = newMemberJoins([{ user_id: "u1", joined_at: JOINED }], "owner");
  assert.equal(isNewMemberMessage(joins, "u1", JOINED_MS - 60_000), true);
  assert.equal(isNewMemberMessage(joins, "u1", JOINED_MS - 24 * 60 * 60 * 1000), false);
});

test("the creator, strangers and unparseable rows are never tagged", () => {
  const joins = newMemberJoins(
    [
      { user_id: "owner", joined_at: JOINED },
      { user_id: "u2", joined_at: "" },
    ],
    "owner",
  );
  assert.equal(isNewMemberMessage(joins, "owner", JOINED_MS), false);
  assert.equal(isNewMemberMessage(joins, "u2", JOINED_MS), false);
  assert.equal(isNewMemberMessage(joins, "someone-else", JOINED_MS), false);
});

test("mobile carries the same rule", () => {
  const desktop = readFileSync(new URL("../src/utils/newMember.ts", import.meta.url), "utf8");
  const mobile = readFileSync(new URL("../../mobile/lib/newMember.ts", import.meta.url), "utf8");
  const body = (src: string) => src.slice(src.indexOf("export const NEW_MEMBER_WINDOW_MS"));
  assert.equal(body(mobile), body(desktop), "mobile/lib/newMember.ts drifted from the desktop rule");
});
