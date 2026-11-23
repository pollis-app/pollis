// A profile change elsewhere (#2084): the DS's content-free nudge decodes only from the DS, typing
// names move with the rename, and the handler re-reads the profile before every named list.
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { decodeRealtimeEvent, isTrustedOrigin } from "../lib/realtime/events.ts";
import { createParticipantResolver } from "../lib/realtime/participants.ts";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const read = (p: string) => readFileSync(join(root, p), "utf8");

test("profile_changed decodes with its user, and only from the DS", () => {
  const event = decodeRealtimeEvent(JSON.stringify({ type: "profile_changed", user_id: "u-ann" }));
  assert.deepEqual(event, { type: "profile_changed", user_id: "u-ann" });
  assert.equal(isTrustedOrigin(event!, false), true);
  assert.equal(isTrustedOrigin(event!, true), false, "a participant cannot fake the DS nudge");
});

test("a profile_changed with no user, or extra fields, carries only the user id", () => {
  assert.equal(decodeRealtimeEvent(JSON.stringify({ type: "profile_changed" })), null);
  assert.equal(decodeRealtimeEvent(JSON.stringify({ type: "profile_changed", user_id: "" })), null);
  const extra = decodeRealtimeEvent(
    JSON.stringify({ type: "profile_changed", user_id: "u-ann", username: "spoofed" }),
  );
  assert.deepEqual(extra, { type: "profile_changed", user_id: "u-ann" });
});

test("a rename moves the typing name of every resolved participant of that user", async () => {
  const resolve = createParticipantResolver(async (_room, ids) =>
    ids.map((identity) => ({ identity, userId: identity.startsWith("r-a") ? "u-ann" : "u-bo", username: "old" })),
  );
  await resolve("room-1", "r-a1");
  await resolve("room-2", "r-a2");
  await resolve("room-1", "r-b1");
  resolve.renameUser("u-ann", "ann-new");
  assert.deepEqual(await resolve("room-1", "r-a1"), { userId: "u-ann", username: "ann-new" });
  assert.deepEqual(await resolve("room-2", "r-a2"), { userId: "u-ann", username: "ann-new" });
  assert.deepEqual(await resolve("room-1", "r-b1"), { userId: "u-bo", username: "old" });
});

test("a subscription knows when its own room entry was dropped, so the inbox re-joins it", () => {
  // The chat can re-open a room the inbox's connect failed on; that new entry lacks the inbox's
  // listener, so asking "is the room open" kept the inbox deaf to the group's nudges.
  const client = read("lib/realtime/client.ts");
  assert.match(client, /isOpen\(\) \{\s*return !closed && entries\.get\(roomName\) === joined;/);
  assert.doesNotMatch(client, /export function isRoomOpen/);
  const realtime = read("hooks/useInboxRealtime.ts");
  assert.match(realtime, /case "membership_changed":[\s\S]*?reopenDropped\(subsRef\.current, handlerRef\.current\);/);
  assert.match(realtime, /reopenDropped\(subs, handleEvent\);/);
  // The join re-reads the lists, and that re-read is what retries a refused room.
  assert.match(realtime, /\}, \[userId, roomsKey, groupsReadAt, dmsReadAt\]\);/);
});

test("the inbox handler refreshes the profile, then every list that names people", () => {
  const realtime = read("hooks/useInboxRealtime.ts");
  assert.match(realtime, /case "profile_changed":[\s\S]*?void onProfileChanged\(qc, event\.user_id\);/);
  // Our own handle is restored from the accounts index: landing and every resume re-read it.
  assert.match(realtime, /void refreshProfile\(landedUser\)/);
  assert.match(realtime, /void refreshProfile\(resumedUser\)/);
  const hook = read("hooks/queries/useUserProfile.ts");
  assert.match(hook, /export async function onProfileChanged[\s\S]*?await refreshProfile\(userId\);[\s\S]*?invalidateQueries/);
  assert.match(hook, /invoke<[^>]*>\(\s*"get_user_profile"/);
  assert.match(hook, /appStore\.setUsername\(profile\.username\)/);
  for (const prefix of ['["messages"]', '["dm"]', '["groups"]', '["user"]', '["pins"]', '["search"]']) {
    assert.ok(hook.includes(prefix), `profile_changed must re-read ${prefix}`);
  }
});
