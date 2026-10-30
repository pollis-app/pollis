/*
 * Safety-number change notices (#1605) sit in the DM timeline where the change
 * was detected, never at the edge of whatever page happens to be loaded.
 */

import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

import { placeKeyChangeNotices } from "../src/utils/keyChangeTimeline.ts";

const at = (iso: string) => Date.parse(iso);
const n = (id: number, detected_at: string) => ({ id, peer_user_id: "bob", detected_at });

test("inside the loaded window: shown, with its time, oldest first", () => {
  const placed = placeKeyChangeNotices(
    [n(2, "2026-10-30T12:00:00.500Z"), n(1, "2026-10-30T11:00:00.000Z")],
    at("2026-10-30T10:00:00Z"),
    at("2026-10-30T13:00:00Z"),
    false,
    false,
  );
  assert.deepEqual(placed.map((p) => p.notice.id), [1, 2]);
  assert.equal(placed[1].at, at("2026-10-30T12:00:00.500Z"));
});

test("older than the loaded page waits for that page; newer than an anchored window waits too", () => {
  const notices = [n(1, "2026-10-30T09:00:00Z"), n(2, "2026-10-30T14:00:00Z")];
  const oldest = at("2026-10-30T10:00:00Z");
  const newest = at("2026-10-30T13:00:00Z");
  assert.deepEqual(placeKeyChangeNotices(notices, oldest, newest, true, true), []);
  // Everything loaded: the change before the first message shows first, the one after the last shows last.
  assert.deepEqual(
    placeKeyChangeNotices(notices, oldest, newest, false, false).map((p) => p.notice.id),
    [1, 2],
  );
});

test("an empty conversation still shows its notices; a bad timestamp never does", () => {
  const placed = placeKeyChangeNotices(
    [n(1, "2026-10-30T09:00:00Z"), n(2, "not a time")],
    null,
    null,
    true,
    true,
  );
  assert.deepEqual(placed.map((p) => p.notice.id), [1]);
});

test("same instant: ordered by id, so a re-render never reshuffles", () => {
  const placed = placeKeyChangeNotices(
    [n(5, "2026-10-30T09:00:00Z"), n(3, "2026-10-30T09:00:00Z")],
    null,
    null,
    false,
    false,
  );
  assert.deepEqual(placed.map((p) => p.notice.id), [3, 5]);
});

test("mobile carries the same rule", () => {
  const desktop = readFileSync(new URL("../src/utils/keyChangeTimeline.ts", import.meta.url), "utf8");
  const mobile = readFileSync(new URL("../../mobile/lib/keyChangeTimeline.ts", import.meta.url), "utf8");
  const body = (src: string) => src.slice(src.indexOf("export interface TimedNotice"));
  assert.equal(body(mobile), body(desktop), "mobile/lib/keyChangeTimeline.ts drifted from the desktop rule");
});
