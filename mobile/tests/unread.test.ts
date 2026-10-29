/*
 * Unread badges from the persisted read cursors (#1422), `mobile/lib/unread.ts`.
 *
 * Mobile's badges used to be in-memory only and bumped only by a live wake-up,
 * so a message that arrived while the app was backgrounded or killed (the push
 * case) never badged, and a cold start wiped them all. They now come from
 * `get_unread_counts`; these pin how that answer becomes the badge set, plus
 * the wiring that makes the refetch happen at the moments that matter.
 */

import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { resumedToForeground, unreadBadges } from "../lib/unread.ts";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const read = (p: string) => readFileSync(join(root, p), "utf8");

test("the core's answer becomes the badge set", () => {
  assert.deepEqual(
    unreadBadges(
      [
        { conversation_id: "dm-1", count: 2 },
        { conversation_id: "chan-1", count: 1 },
      ],
      null,
    ),
    { "dm-1": 2, "chan-1": 1 },
  );
});

test("a conversation absent from the answer has no badge (replace, not merge)", () => {
  // Another device read it: the core no longer lists it, so a merge would
  // strand the old badge here. The store replaces its whole set with this.
  assert.deepEqual(unreadBadges([{ conversation_id: "dm-1", count: 3 }], null), {
    "dm-1": 3,
  });
  assert.deepEqual(unreadBadges([], null), {});
  assert.deepEqual(unreadBadges(null, null), {});
  assert.deepEqual(unreadBadges(undefined, null), {});
});

test("the conversation on screen never badges", () => {
  // The answer can be read before the open chat's cursor advanced; showing it
  // would flash a badge for the very conversation the user is reading.
  assert.deepEqual(
    unreadBadges(
      [
        { conversation_id: "dm-1", count: 2 },
        { conversation_id: "chan-1", count: 4 },
      ],
      "dm-1",
    ),
    { "chan-1": 4 },
  );
});

test("a zero or negative count is no badge", () => {
  assert.deepEqual(
    unreadBadges(
      [
        { conversation_id: "a", count: 0 },
        { conversation_id: "b", count: -1 },
        { conversation_id: "c", count: 1 },
      ],
      null,
    ),
    { c: 1 },
  );
});

test("only a return to the foreground counts as a resume", () => {
  assert.equal(resumedToForeground("background", "active"), true);
  assert.equal(resumedToForeground("inactive", "active"), true);
  assert.equal(resumedToForeground("active", "active"), false);
  assert.equal(resumedToForeground("active", "background"), false);
  assert.equal(resumedToForeground("active", "inactive"), false);
});

test("the store hydrates through unreadBadges, filtering the open conversation", () => {
  const store = read("stores/appStore.ts");
  assert.match(store, /hydrateUnread\(counts: UnreadCount\[\]\)/);
  assert.match(store, /unreadBadges\(counts, this\.openConversationId\)/);
  // Sign-out forgets which conversation was open.
  assert.match(store, /this\.openConversationId = null;/);
});

test("the persisted counts are read with the desktop command shapes", () => {
  const hook = read("hooks/queries/useUnread.ts");
  assert.match(hook, /invoke<UnreadCount\[\]>\("get_unread_counts", \{ userId \}\)/);
  assert.match(hook, /invoke<boolean>\("mark_conversation_read", \{ conversationId, userId \}\)/);
  assert.match(hook, /invoke<number>\("sync_read_cursors", \{ userId \}\)/);
  // No polling: refetches are event-driven (unlock, foreground, ingest).
  assert.doesNotMatch(hook, /refetchInterval|setInterval/);
  // An answer equal to the last must still re-hydrate: the store may have
  // moved in between (a live bump, an optimistic clear).
  assert.match(hook, /structuralSharing: false/);
  // Gated on being past the PIN: the counts live in the encrypted local DB.
  assert.match(hook, /isInSignedInApp\(segments, isLocked\)/);
  assert.match(hook, /AppState\.addEventListener\("change"/);
});

test("every ingest re-reads the counts unless the caller bumps them itself", () => {
  const messages = read("hooks/queries/useMessages.ts");
  assert.match(messages, /if \(opts\.refreshUnread !== false\) \{\s*refreshUnread\(queryClient\);/);
  // The live wake-up keeps its instant bump and re-reads after it.
  const realtime = read("hooks/useInboxRealtime.ts");
  assert.match(realtime, /ingestRef\.current\(id, kind, \{ refreshUnread: false \}\)/);
  assert.match(realtime, /appStore\.incrementUnread\(id\);\s*\}\s*refreshUnread\(qc\);/);
});

test("every place that cleared a badge also advances the persisted cursor", () => {
  for (const p of [
    "app/group/[id].tsx",
    "components/search/GlobalSearchResults.tsx",
    "app/(tabs)/direct.tsx",
    "app/(tabs)/groups.tsx",
    "hooks/useOpenConversation.ts",
  ]) {
    const src = read(p);
    assert.doesNotMatch(src, /appStore\.markRead\(/, `${p} clears only the in-memory badge`);
    assert.match(src, /markConversationRead\(/, `${p} should mark the conversation read`);
  }
  // The chat screen marks itself read however it was reached (a notification
  // tap included) and records itself as the open conversation.
  const chat = read("app/chat/[id].tsx");
  assert.match(chat, /appStore\.setOpenConversationId\(conversationId\);\s*markConversationRead\(conversationId\);/);
  assert.match(chat, /appStore\.clearOpenConversationId\(conversationId\);/);
});

test("landing in the app catches up every listed conversation once", () => {
  // A message that arrived while the app was closed woke nothing: without a
  // catch-up on landing it was never ingested, so it could not be counted.
  const realtime = read("hooks/useInboxRealtime.ts");
  assert.match(
    realtime,
    /landedRef\.current = true;[\s\S]{0,400}?catchUpThenBackstop\(\s*catchUpTargets\(groupsRef\.current, dmsRef\.current\)/,
  );
  assert.match(realtime, /if \(!inApp\) \{\s*landedRef\.current = false;/);
});

test("the unread lifecycle is mounted with the signed-in services", () => {
  const layout = read("app/_layout.tsx");
  assert.match(layout, /useUnreadSync\(\);/);
});
