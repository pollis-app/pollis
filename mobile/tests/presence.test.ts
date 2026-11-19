/* Presence dots (#2059): desktop's rule, the store, the realtime feed, and one avatar for every user. */

import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync, readdirSync, statSync } from "node:fs";
import { dirname, join, relative } from "node:path";
import { fileURLToPath } from "node:url";
import { autorun } from "mobx";

import {
  createRoomPresenceFeed,
  onlineUsers,
  presenceDotGeometry,
  statusOf,
  withRoom,
  withoutRoom,
  type RoomPresence,
} from "../lib/presence.ts";
import { presenceStore } from "../stores/presenceStore.ts";

const ROOT = join(dirname(fileURLToPath(import.meta.url)), "..");
const src = (rel: string) => readFileSync(join(ROOT, rel), "utf8");

function sources(): string[] {
  const out: string[] = [];
  const walk = (dir: string) => {
    for (const name of readdirSync(dir)) {
      const full = join(dir, name);
      if (statSync(full).isDirectory()) {
        walk(full);
      } else if (/\.tsx?$/.test(name)) {
        out.push(relative(ROOT, full));
      }
    }
  };
  for (const dir of ["app", "components"]) {
    walk(join(ROOT, dir));
  }
  return out;
}

// ── the rule ────────────────────────────────────────────────────────────────

test("a user is online while any shared room shows them", () => {
  let s: RoomPresence = {};
  s = withRoom(s, "dm-1", ["bob"]);
  s = withRoom(s, "group-1", ["bob", "carol"]);
  assert.equal(statusOf(onlineUsers(s), "bob", "me"), "online");
  assert.equal(statusOf(onlineUsers(s), "carol", "me"), "online");
  // Bob leaves the DM's room but is still in the group's.
  s = withRoom(s, "dm-1", []);
  assert.equal(statusOf(onlineUsers(s), "bob", "me"), "online");
  s = withRoom(s, "group-1", ["carol"]);
  assert.equal(statusOf(onlineUsers(s), "bob", "me"), "offline");
  assert.equal(statusOf(onlineUsers(s), "carol", "me"), "online");
});

test("a room this device left vouches for no one", () => {
  let s: RoomPresence = withRoom({}, "group-1", ["bob"]);
  s = withoutRoom(s, "group-1");
  assert.equal(statusOf(onlineUsers(s), "bob", "me"), "offline");
  assert.equal(withoutRoom(s, "never-joined"), s, "an unknown room changes nothing");
});

test("self is online; no user is no one", () => {
  const none = onlineUsers({});
  assert.equal(statusOf(none, "me", "me"), "online");
  assert.equal(statusOf(none, null, "me"), "offline");
  assert.equal(statusOf(none, undefined, null), "offline");
  assert.equal(statusOf(none, "bob", null), "offline");
});

test("a snapshot lists each user once", () => {
  assert.deepEqual(withRoom({}, "r", ["bob", "bob"]).r, ["bob"]);
});

// ── the store ───────────────────────────────────────────────────────────────

test("the store re-renders readers as rooms change, and forgets everything on sign-out", () => {
  presenceStore.reset();
  presenceStore.setSelfUserId("me");
  const seen: string[] = [];
  const dispose = autorun(() => {
    seen.push(presenceStore.status("bob"));
  });
  presenceStore.setRoom("dm-1", ["bob"]);
  presenceStore.setRoom("group-1", ["bob"]);
  presenceStore.clearRoom("dm-1");
  presenceStore.clearRoom("group-1");
  dispose();
  // The second room and the first clear leave Bob online: no re-render for them.
  assert.deepEqual(seen, ["offline", "online", "offline"]);

  assert.equal(presenceStore.status("me"), "online");
  presenceStore.setRoom("group-1", ["bob"]);
  presenceStore.reset();
  assert.equal(presenceStore.status("bob"), "offline");
  assert.equal(presenceStore.status("me"), "offline", "no self after sign-out");
});

test("sign-in and sign-out reach the store through appStore", () => {
  const app = src("stores/appStore.ts");
  assert.match(app, /setCurrentUser\(user: User \| null\) \{\s*this\.currentUser = user;\s*presenceStore\.setSelfUserId\(user\?\.id \?\? null\);/);
  assert.match(app, /logout\(\) \{[\s\S]*presenceStore\.reset\(\);/);
});

// ── the realtime feed ───────────────────────────────────────────────────────

function deferred<T>() {
  let resolve!: (v: T) => void;
  let reject!: (e: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

function recordingSink() {
  const calls: string[] = [];
  return {
    calls,
    setRoom(room: string, users: string[]) {
      calls.push(`set ${room} ${users.join(",")}`);
    },
    clearRoom(room: string) {
      calls.push(`clear ${room}`);
    },
  };
}

test("the feed hands the core the room's identities and stores its answer", async () => {
  const asked: Array<[string, string[]]> = [];
  const sink = recordingSink();
  const feed = createRoomPresenceFeed(async (room, ids) => {
    asked.push([room, ids]);
    return ["bob"];
  }, sink);
  await feed.refresh("dm-1", ["d-abc", "d-def"]);
  assert.deepEqual(asked, [["dm-1", ["d-abc", "d-def"]]]);
  assert.deepEqual(sink.calls, ["set dm-1 bob"]);
});

test("only the newest refresh of a room lands", async () => {
  const sink = recordingSink();
  const first = deferred<string[]>();
  const second = deferred<string[]>();
  const answers = [first, second];
  const feed = createRoomPresenceFeed(() => answers.shift()!.promise, sink);
  const a = feed.refresh("dm-1", ["d-a"]);
  const b = feed.refresh("dm-1", []);
  second.resolve([]);
  await b;
  first.resolve(["bob"]);
  await a;
  assert.deepEqual(sink.calls, ["set dm-1 "], "the stale answer that Bob is there is dropped");
});

test("nothing lands for a room after it was dropped", async () => {
  const sink = recordingSink();
  const answer = deferred<string[]>();
  const feed = createRoomPresenceFeed(() => answer.promise, sink);
  const pending = feed.refresh("dm-1", ["d-a"]);
  feed.drop("dm-1");
  answer.resolve(["bob"]);
  await pending;
  assert.deepEqual(sink.calls, ["clear dm-1"]);
});

test("a failed resolve (offline, or a binary without the command) leaves the room as it was", async () => {
  const sink = recordingSink();
  const feed = createRoomPresenceFeed(() => Promise.reject(new Error("unknown command")), sink);
  await feed.refresh("dm-1", ["d-a"]);
  assert.deepEqual(sink.calls, []);
});

test("the realtime client feeds presence from the core on every roster change and drops a closed room", () => {
  const client = src("lib/realtime/client.ts");
  assert.match(client, /invoke<string\[\]>\("livekit_room_presence", \{ room, identities \}\)/);
  for (const ev of ["Reconnected", "ParticipantConnected", "ParticipantDisconnected"]) {
    assert.match(client, new RegExp(`RoomEvent\\.${ev}, \\(\\) => \\{\\s*refreshPresence\\(roomName, room\\);`));
  }
  assert.match(client, /entry\.room = room;\s*refreshPresence\(roomName, room\);/);
  assert.match(client, /presenceFeed\.drop\(roomName\);\s*void entry\.room\.disconnect\(\);/);
});

// ── the dot and the one avatar ──────────────────────────────────────────────

test("the dot is desktop's: a quarter of the avatar, never under 5, a 2-point ring past the corner", () => {
  assert.deepEqual(presenceDotGeometry(40), { dot: 10, ring: 2, offset: -2 });
  assert.deepEqual(presenceDotGeometry(64), { dot: 15, ring: 2, offset: -2 });
  assert.deepEqual(presenceDotGeometry(16), { dot: 5, ring: 2, offset: -2 });
});

test("the dot draws in theme colours only: accent on the surface behind, hollow when offline", () => {
  const avatar = src("components/UserAvatar.tsx");
  const dot = avatar.slice(avatar.indexOf("function PresenceDot"), avatar.indexOf("/**\n * Every user avatar"));
  assert.match(dot, /borderColor: online \? ring : semantic\.accentMuted/);
  assert.match(dot, /backgroundColor: online \? semantic\.accent : semantic\.bg/);
  assert.doesNotMatch(dot, /#[0-9a-f]{3,8}\b|rgba?\(|shadow|elevation/i, "solid colours, no glow");
  assert.match(avatar, /ring=\{semantic\[on\]\}/);
  // Only a user gets a dot.
  assert.match(avatar, /const drawsDot = presence && !!userId;/);
});

test("Maestro sees the dot's state through the screen probe", () => {
  const avatar = src("components/UserAvatar.tsx");
  assert.match(avatar, /useReportTag\(drawsDot && testID \? `\$\{testID\}-presence-\$\{status\}` : null\);/);
});

test("every user avatar is the one UserAvatar; the letter tile is drawn directly only for groups", () => {
  const offenders = sources().filter((file) => {
    if (file === join("components", "UserAvatar.tsx") || file === join("components", "ui.tsx")) {
      return false;
    }
    const s = src(file);
    const letterTiles = [...s.matchAll(/<Avatar\b[^>]*>/g)].map((m) => m[0]);
    // A group or channel is drawn as a rounded letter tile, never a person.
    return letterTiles.some((tag) => !/shape="rounded"/.test(tag));
  });
  assert.deepEqual(offenders, [], "draw a user with UserAvatar");
});

test("editors draw their just-picked picture through UserAvatar, not a hand-made circle", () => {
  for (const file of ["app/self/alias.tsx", "components/self/ProfilePhotoEditor.tsx"]) {
    const s = src(file);
    assert.doesNotMatch(s, /from "expo-image"/, file);
    assert.match(s, /<UserAvatar[\s\S]*?previewUri=\{preview\}/, file);
  }
});

test("a group DM's stacked pair stands for the conversation: no dots", () => {
  const row = src("components/direct/ConversationRow.tsx");
  const pair = row.slice(row.indexOf("if (people.length < 2)"));
  const single = pair.slice(0, pair.indexOf("return (\n    <View"));
  assert.doesNotMatch(single, /presence=\{false\}/, "a one-to-one DM shows the person's dot");
  assert.equal(pair.match(/presence=\{false\}/g)?.length, 2);
});
