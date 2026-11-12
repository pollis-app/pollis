// A missed wake-up is caught up; a wake never counts unread itself (#1628).
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { catchUpTargets, roomCatchUpTarget } from "../lib/messageWake.ts";
import { decodeRealtimeEvent, isTrustedOrigin } from "../lib/realtime/events.ts";
import { createReadCursorConverge } from "../lib/unread.ts";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const read = (p: string) => readFileSync(join(root, p), "utf8");

test("a wake ingests and re-reads Rust's counts; it never bumps a badge or reads the newest row", () => {
  const realtime = read("hooks/useInboxRealtime.ts");
  const wake = realtime.slice(
    realtime.indexOf('case "new_message"'),
    realtime.indexOf('case "realtime_reconnected"'),
  );
  assert.match(wake, /ingestRef\.current\(id, event\.channel_id \? "channel" : "dm"\)/);
  assert.doesNotMatch(wake, /incrementUnread|read_last_messages/);
  assert.doesNotMatch(read("stores/appStore.ts"), /incrementUnread/);
  // The ingest itself re-reads the counts.
  assert.match(read("hooks/queries/useMessages.ts"), /refreshUnread\(queryClient\);/);
});

// The prod repro of #1628: the message is here and unread; the user reads it on another device,
// whose read receipt wakes this phone, then the DS nudges. The receipt wake only ingests (above);
// the nudge's converge re-reads the badges and clears the conversation's notifications.
test("the other device's read: its receipt wake adds nothing, its nudge clears badge and tray", async () => {
  const realtime = read("hooks/useInboxRealtime.ts");
  const wake = realtime.slice(
    realtime.indexOf('case "new_message"'),
    realtime.indexOf('case "realtime_reconnected"'),
  );
  // The whole wake: one ingest, nothing else that could alert or count.
  assert.equal((wake.match(/ingestRef\.current\(/g) ?? []).length, 1);
  assert.doesNotMatch(wake, /notify|present|schedule|increment|appStore/i);

  const refreshed: number[] = [];
  const dismissed: string[] = [];
  let answer: string[] = ["dm-1"];
  const converge = createReadCursorConverge({
    refresh: async () => answer,
    refreshUnread: () => {
      refreshed.push(1);
    },
    dismiss: async (id) => {
      dismissed.push(id);
    },
    warn: () => {},
  });
  assert.deepEqual(await converge(), ["dm-1"]);
  assert.equal(refreshed.length, 1, "the badges are re-read from Rust");
  assert.deepEqual(dismissed, ["dm-1"], "the read conversation's notifications are cleared");

  // A nudge that moves nothing (this device's own push) clears nothing.
  answer = [];
  assert.deepEqual(await converge(), []);
  assert.equal(refreshed.length, 1);
  assert.deepEqual(dismissed, ["dm-1"]);
});

test("concurrent converges share one run plus one trailing run", async () => {
  let calls = 0;
  let release: () => void = () => {};
  const gate = new Promise<void>((r) => {
    release = r;
  });
  const converge = createReadCursorConverge({
    refresh: async () => {
      calls++;
      await gate;
      return [];
    },
    refreshUnread: () => {},
    dismiss: async () => {},
    warn: () => {},
  });
  const a = converge();
  const b = converge();
  const c = converge();
  release();
  await Promise.all([a, b, c]);
  assert.equal(calls, 2);
});

test("the DS read-cursor nudge decodes, and only from the DS", () => {
  const event = decodeRealtimeEvent(JSON.stringify({ type: "read_cursors_changed" }));
  assert.ok(event, "read_cursors_changed must decode");
  assert.equal(isTrustedOrigin(event, false), true);
  assert.equal(isTrustedOrigin(event, true), false);
});

test("the nudge and a reconnect converge cursors and dismiss what was read elsewhere", () => {
  const realtime = read("hooks/useInboxRealtime.ts");
  assert.match(realtime, /case "read_cursors_changed":\s*\/\/[^\n]*\n\s*void refreshReadCursors\(userId, qc\);/);
  assert.match(realtime, /case "realtime_reconnected":[\s\S]*?refreshReadCursors\(userId, qc\)/);
  const hook = read("hooks/queries/useUnread.ts");
  assert.match(hook, /invoke<string\[\]>\("refresh_read_cursors", \{ userId \}\)/);
  assert.match(hook, /dismissConversationNotifications\(id\)/);
});

const groups = [
  { id: "g1", channels: [{ id: "g1-general" }, { id: "g1-random" }] },
  { id: "g2", channels: [] },
];
const dms = [{ id: "dm1" }];

test("resume catches up one ingest per group and one per DM", () => {
  assert.deepEqual(catchUpTargets(groups, dms), [
    { id: "g1-general", kind: "channel" },
    { id: "dm1", kind: "dm" },
  ]);
});

test("a reconnected room maps to the ingest that covers it", () => {
  assert.deepEqual(roomCatchUpTarget("g1", groups, dms), { id: "g1-general", kind: "channel" });
  assert.deepEqual(roomCatchUpTarget("dm1", groups, dms), { id: "dm1", kind: "dm" });
  assert.equal(roomCatchUpTarget("inbox-01SELF", groups, dms), null);
  assert.equal(roomCatchUpTarget("g2", groups, dms), null);
  assert.equal(roomCatchUpTarget("unknown", groups, dms), null);
});
