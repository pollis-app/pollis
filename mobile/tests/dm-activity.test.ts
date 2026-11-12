/* The DM list's live re-sort (#1838): a sent or received message moves its DM to the top, an older
   stamp moves nothing, ties break by id as in the core, and mobile's copy is desktop's file exactly. */
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { activityMs, bumpActivity } from "../lib/dmActivity.ts";

const here = dirname(fileURLToPath(import.meta.url));

const ids = (rows: { id: string }[] | undefined) => (rows ?? []).map((r) => r.id);

// As `list_dm_channels` returns them: newest activity first.
const listed = () => [
  { id: "dm-c", updated_at: 3_000 },
  { id: "dm-b", updated_at: 2_000 },
  { id: "dm-a", updated_at: 1_000 },
];

test("mobile/lib/dmActivity.ts is desktop's dmActivity.ts, byte for byte", () => {
  const mobile = readFileSync(join(here, "../lib/dmActivity.ts"), "utf8");
  const desktop = readFileSync(join(here, "../../frontend/src/utils/dmActivity.ts"), "utf8");
  assert.equal(mobile, desktop, "edit frontend/src/utils/dmActivity.ts and copy it to mobile/lib/dmActivity.ts");
});

test("a new message in the oldest DM moves it to the top", () => {
  const next = bumpActivity(listed(), "dm-a", 4_000);
  assert.deepEqual(ids(next), ["dm-a", "dm-c", "dm-b"]);
  assert.equal(next?.[0].updated_at, 4_000);
});

test("an older stamp (an edit, a replay, opening an old DM) changes nothing", () => {
  const rows = listed();
  assert.equal(bumpActivity(rows, "dm-a", 500), rows);
  assert.equal(bumpActivity(rows, "dm-a", 1_000), rows);
});

test("an unlisted DM or no list yet is left alone", () => {
  const rows = listed();
  assert.equal(bumpActivity(rows, "dm-gone", 9_000), rows);
  assert.equal(bumpActivity(undefined, "dm-a", 9_000), undefined);
});

test("ties break by id descending, the core's order", () => {
  const rows = [
    { id: "dm-c", updated_at: 2_000 },
    { id: "dm-b", updated_at: 1_000 },
    { id: "dm-a", updated_at: 500 },
  ];
  // dm-a now ties dm-c; the higher id stays first.
  assert.deepEqual(ids(bumpActivity(rows, "dm-a", 2_000)), ["dm-c", "dm-a", "dm-b"]);
  assert.deepEqual(ids(bumpActivity(rows, "dm-b", 2_000)), ["dm-c", "dm-b", "dm-a"]);
});

test("stamps read as instants; an unreadable one sorts last", () => {
  assert.equal(activityMs("2026-01-01T10:00:00Z"), activityMs("2026-01-01T10:00:00+00:00"));
  assert.equal(activityMs("2026-01-01T06:00:00-05:00"), Date.UTC(2026, 0, 1, 11));
  assert.equal(activityMs("not a time"), 0);
  assert.equal(activityMs(undefined), 0);
});
