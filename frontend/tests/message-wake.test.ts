/* #1628: a `new_message` wake alerts and counts only real news — what Rust's ingest reports as
   newly stored, someone else's and unread. Receipts, replays and messages read on another device
   report nothing. Unread is refetched from Rust, never incremented locally. */
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import { isOwn, runMessageWake, wakeAlerts, type WakeFresh } from "../src/utils/messageWake.ts";

const SELF = "01SELF";
const PEER = "01PEER";
const DM = "dm-1";

function wake(fresh: WakeFresh[] | null | Error, selected: string | null = null) {
  const announced: WakeFresh[] = [];
  let refreshed = 0;
  const done = runMessageWake({
    ingest: async () => {
      if (fresh instanceof Error) {
        throw fresh;
      }
      return fresh;
    },
    refreshUnread: () => {
      refreshed++;
    },
    selfId: SELF,
    isSelected: (id) => id === selected,
    announce: (m) => {
      announced.push(m);
    },
  });
  return done.then(() => ({ announced, refreshed }));
}

test("a receipt wake (ingest stored nothing) neither alerts nor counts, but refetches unread", async () => {
  const r = await wake([]);
  assert.deepEqual(r.announced, []);
  assert.equal(r.refreshed, 1);
});

test("a message from someone else that the ingest reports alerts once", async () => {
  const m = { conversation_id: DM, sender_id: PEER, sender_username: "peer" };
  const r = await wake([m]);
  assert.deepEqual(r.announced, [m]);
});

test("the conversation on screen never alerts", async () => {
  const r = await wake([{ conversation_id: DM, sender_id: PEER }], DM);
  assert.deepEqual(r.announced, []);
});

test("a failed ingest stored nothing, so it announces nothing and still refetches", async () => {
  const r = await wake(new Error("offline"));
  assert.deepEqual(r.announced, []);
  assert.equal(r.refreshed, 1);
  const n = await wake(null);
  assert.deepEqual(n.announced, []);
});

test("our own message is never news, even if reported", () => {
  assert.deepEqual(
    wakeAlerts({ fresh: [{ conversation_id: DM, sender_id: SELF }], selfId: SELF, isSelected: () => false }),
    [],
  );
  assert.equal(isOwn({ sender_id: SELF }, null), false);
  assert.equal(isOwn(undefined, SELF), false);
});

test("a channel wake announces each sibling channel the group ingest reported", async () => {
  const a = { conversation_id: "c-a", sender_id: PEER };
  const b = { conversation_id: "c-b", sender_id: PEER };
  const r = await wake([a, b], "c-b");
  assert.deepEqual(r.announced, [a]);
});

const SRC = (p: string) => readFileSync(fileURLToPath(new URL(`../src/${p}`, import.meta.url)), "utf8");

test("GUARD: no local unread increment survives anywhere in the renderer (#1628)", () => {
  for (const file of ["stores/appStore.ts", "utils/notify.ts", "hooks/useLiveKitRealtime.ts"]) {
    assert.ok(!/incrementUnread/.test(SRC(file)), `${file} still increments unread locally`);
  }
});

test("GUARD: the wake handler decides from the ingest result, not the newest row", () => {
  const handler = SRC("hooks/useLiveKitRealtime.ts");
  assert.ok(/runMessageWake\(/.test(handler));
  assert.ok(!/read_last_messages/.test(handler), "the newest row is not news: it may be old or read");
});

test("GUARD: cursors re-sync on the DS nudge and on reconnect", () => {
  const handler = SRC("hooks/useLiveKitRealtime.ts");
  assert.match(handler, /event\.type === 'read_cursors_changed'[\s\S]*?refreshReadCursors/);
  assert.match(handler, /event\.type === 'realtime_reconnected'[\s\S]*?refreshReadCursors/);
  assert.match(SRC("hooks/queries/useUnread.ts"), /addEventListener\("focus"/);
});
