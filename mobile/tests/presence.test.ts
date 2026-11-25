/* Presence dots (#2059) and away (#2110): desktop's rule, the store, the realtime feed, this
   device's `active` attribute, and one avatar for every user. */

import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync, readdirSync, statSync } from "node:fs";
import { dirname, join, relative } from "node:path";
import { fileURLToPath } from "node:url";
import { autorun } from "mobx";

import {
  activeAttributes,
  appStateInUse,
  awayBite,
  createRoomPresenceFeed,
  presentUsers,
  presenceDotGeometry,
  sessionInUse,
  snapshotOf,
  statusOf,
  withRoom,
  withoutRoom,
  type RoomPresence,
  type RoomSnapshot,
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

const on = (...online: string[]): RoomSnapshot => ({ online, away: [] });
const status = (s: RoomPresence, user: string) => statusOf(presentUsers(s), user, "me");

test("a user is online while any shared room shows them", () => {
  let s: RoomPresence = {};
  s = withRoom(s, "dm-1", on("bob"));
  s = withRoom(s, "group-1", on("bob", "carol"));
  assert.equal(status(s, "bob"), "online");
  assert.equal(status(s, "carol"), "online");
  // Bob leaves the DM's room but is still in the group's.
  s = withRoom(s, "dm-1", on());
  assert.equal(status(s, "bob"), "online");
  s = withRoom(s, "group-1", on("carol"));
  assert.equal(status(s, "bob"), "offline");
  assert.equal(status(s, "carol"), "online");
});

test("#2110: away while every room that shows them shows them not in use; online if any does", () => {
  let s: RoomPresence = withRoom({}, "dm-1", { online: [], away: ["bob"] });
  assert.equal(status(s, "bob"), "away");
  s = withRoom(s, "group-1", on("bob"));
  assert.equal(status(s, "bob"), "online");
  s = withRoom(s, "group-1", { online: [], away: ["bob"] });
  assert.equal(status(s, "bob"), "away");
  s = withoutRoom(s, "dm-1");
  s = withoutRoom(s, "group-1");
  assert.equal(status(s, "bob"), "offline");
  // Listed both ways in one room: online.
  assert.deepEqual(withRoom({}, "r", { online: ["bob"], away: ["bob"] }).r, on("bob"));
});

test("#2110: a missing attribute is in use, so a build that predates it still shows online", () => {
  assert.equal(sessionInUse(undefined), true);
  assert.equal(sessionInUse({}), true);
  assert.equal(sessionInUse({ active: "1" }), true);
  assert.equal(sessionInUse({ active: "0" }), false);
  // A core that predates #2110 answers a bare list: everyone online.
  assert.deepEqual(snapshotOf(["bob"]), on("bob"));
  assert.deepEqual(snapshotOf({ online: ["bob"], away: ["carol"] }), { online: ["bob"], away: ["carol"] });
  assert.deepEqual(snapshotOf(null), on());
});

test("#2110: this device is not in use only in the background", () => {
  assert.equal(appStateInUse("active"), true);
  assert.equal(appStateInUse("inactive"), true, "iOS's app switcher passes in a moment");
  assert.equal(appStateInUse("background"), false);
  assert.deepEqual(activeAttributes(true), { active: "1" });
  assert.deepEqual(activeAttributes(false), { active: "0" });
});

test("a room this device left vouches for no one", () => {
  let s: RoomPresence = withRoom({}, "group-1", on("bob"));
  s = withoutRoom(s, "group-1");
  assert.equal(status(s, "bob"), "offline");
  assert.equal(withoutRoom(s, "never-joined"), s, "an unknown room changes nothing");
});

test("self is online; no user is no one", () => {
  const none = presentUsers({});
  assert.equal(statusOf(none, "me", "me"), "online");
  assert.equal(statusOf(none, null, "me"), "offline");
  assert.equal(statusOf(none, undefined, null), "offline");
  assert.equal(statusOf(none, "bob", null), "offline");
});

test("a snapshot lists each user once", () => {
  assert.deepEqual(withRoom({}, "r", on("bob", "bob")).r, on("bob"));
});

// ── the store ───────────────────────────────────────────────────────────────

test("the store re-renders readers as rooms change, and forgets everything on sign-out", () => {
  presenceStore.reset();
  presenceStore.setSelfUserId("me");
  const seen: string[] = [];
  const dispose = autorun(() => {
    seen.push(presenceStore.status("bob"));
  });
  presenceStore.setRoom("dm-1", on("bob"));
  presenceStore.setRoom("group-1", on("bob"));
  presenceStore.setRoom("dm-1", { online: [], away: ["bob"] });
  presenceStore.setRoom("group-1", { online: [], away: ["bob"] });
  presenceStore.clearRoom("dm-1");
  presenceStore.clearRoom("group-1");
  dispose();
  // The second room, and the first going away while the second still shows him in use, leave Bob
  // online: no re-render for them.
  assert.deepEqual(seen, ["offline", "online", "away", "offline"]);

  assert.equal(presenceStore.status("me"), "online");
  presenceStore.setRoom("group-1", on("bob"));
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
    setRoom(room: string, snap: RoomSnapshot) {
      const away = snap.away.length > 0 ? ` away ${snap.away.join(",")}` : "";
      calls.push(`set ${room} ${snap.online.join(",")}${away}`);
    },
    clearRoom(room: string) {
      calls.push(`clear ${room}`);
    },
  };
}

test("the feed hands the core the room's identities and the inactive ones, and stores its answer", async () => {
  const asked: Array<[string, string[], string[]]> = [];
  const sink = recordingSink();
  const feed = createRoomPresenceFeed(async (room, ids, inactive) => {
    asked.push([room, ids, inactive]);
    return { online: ["bob"], away: ["carol"] };
  }, sink);
  await feed.refresh("dm-1", ["d-abc", "d-def"], ["d-def"]);
  assert.deepEqual(asked, [["dm-1", ["d-abc", "d-def"], ["d-def"]]]);
  assert.deepEqual(sink.calls, ["set dm-1 bob away carol"]);
});

test("only the newest refresh of a room lands", async () => {
  const sink = recordingSink();
  const first = deferred<unknown>();
  const second = deferred<unknown>();
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
  const answer = deferred<unknown>();
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
  assert.match(client, /invoke<unknown>\("livekit_room_presence", \{ room, identities, inactive \}\)/);
  assert.match(client, /if \(!sessionInUse\(participant\.attributes\)\) \{\s*inactive\.push\(identity\);/);
  const events = ["Reconnected", "ParticipantConnected", "ParticipantDisconnected", "ParticipantAttributesChanged"];
  for (const ev of events) {
    assert.match(client, new RegExp(`RoomEvent\\.${ev}, \\(\\) => \\{\\s*refreshPresence\\(roomName, room\\);`));
  }
  assert.match(client, /entry\.room = room;\s*refreshPresence\(roomName, room\);/);
  assert.match(client, /presenceFeed\.drop\(roomName\);\s*void entry\.room\.disconnect\(\);/);
});

test("#2110: the client writes this device's attribute on an AppState change and on a join while away", () => {
  const client = src("lib/realtime/client.ts");
  assert.match(client, /room\.localParticipant\.setAttributes\(activeAttributes\(inUse\)\)/);
  assert.match(client, /const now = appStateInUse\(next\);\s*if \(now === inUse\) \{\s*return;\s*\}/);
  assert.match(client, /followAppState\(\);\s*if \(!inUse\) \{\s*publishActivity\(room\);/);
});

// ── the dot and the one avatar ──────────────────────────────────────────────

test("the dot is desktop's: a quarter of the avatar, never under 5, a 2-point ring past the corner", () => {
  assert.deepEqual(presenceDotGeometry(40), { dot: 10, ring: 2, offset: -2 });
  assert.deepEqual(presenceDotGeometry(64), { dot: 15, ring: 2, offset: -2 });
  assert.deepEqual(presenceDotGeometry(16), { dot: 5, ring: 2, offset: -2 });
});

test("away bites four fifths of the dot out of its top corner", () => {
  assert.deepEqual(awayBite(10), { size: 8, inset: -2 });
  assert.deepEqual(awayBite(5), { size: 4, inset: -1 });
});

test("the dot draws in theme colours only: accent on the surface behind, a crescent when away, hollow when offline", () => {
  const avatar = src("components/UserAvatar.tsx");
  const dot = avatar.slice(avatar.indexOf("function PresenceDot"), avatar.indexOf("/**\n * Every user avatar"));
  assert.match(dot, /const online = status !== "offline";/);
  assert.match(dot, /status === "away" \?[\s\S]*backgroundColor: ring,/);
  assert.match(dot, /overflow: "hidden"/);
  assert.match(dot, /borderColor: online \? ring : semantic\.accentMuted/);
  assert.match(dot, /backgroundColor: online \? semantic\.accent : semantic\.bg/);
  assert.doesNotMatch(dot, /#[0-9a-f]{3,8}\b|rgba?\(|shadow|elevation/i, "solid colours, no glow");
  assert.match(avatar, /ring=\{semantic\[on\]\}/);
  // Only a user gets a dot.
  assert.match(avatar, /const drawsDot = presence && !!userId;/);
});

test("Maestro sees the dot's state through the screen probe, under a tag no row selector matches", () => {
  const avatar = src("components/UserAvatar.tsx");
  // A full-screen probe tagged `row-message-…-presence-…` took the long press `row-message-.*` meant
  // for the row, on every flow that opens the message sheet.
  assert.doesNotMatch(avatar, /`\$\{testID\}-presence-/);
  assert.match(avatar, /useReportTag\(drawsDot && testID \? `presence-\$\{status\}-\$\{testID\}` : null\);/);
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

test("starting a DM shows it is busy and takes one tap, so a retap never makes a second", () => {
  const page = src("app/dm/new.tsx");
  // Claimed synchronously (`useSubmitLock`), not read from `isPending`: two taps in one frame both
  // see the render before the mutation started.
  assert.match(page, /if \(!start\.begin\(\)\) \{\s*return;\s*\}/);
  assert.match(page, /onError: \(\) => \{\s*start\.end\(\);/);
  assert.match(page, /disabled=\{start\.busy\}/);
  assert.match(page, /testID="busy-start-dm"/);
});
