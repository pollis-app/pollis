/* Profile pictures: key parsing, the renditions core writes, and when the store fetches or draws. */

import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

import {
  AVATAR_RENDITIONS,
  avatarHash,
  isFileUserId,
  parseRenditionFile,
  renditionFor,
  siblingUri,
} from "../lib/avatarKey.ts";
import { createAvatarStore, PROFILE_BATCH_MAX, type AvatarStoreDeps } from "../lib/avatarStore.ts";

const src = (p: string) => readFileSync(new URL(`../${p}`, import.meta.url), "utf8");

const H1 = "1".repeat(64);
const H2 = "2".repeat(64);
const key = (user: string, hash: string, ext = "jpg") => `avatars/${user}/${hash}.${ext}`;
const file = (user: string, hash: string, px: number) => `${user}_${hash}_${px}.jpg`;
const DIR = "file:///cache/pollis-avatars/";

test("only content-addressed avatar keys name a picture", () => {
  assert.equal(avatarHash(key("u1", H1)), H1);
  assert.equal(avatarHash(key("u1", H1, "png")), H1);
  assert.equal(avatarHash(""), null, "removed");
  assert.equal(avatarHash(null), null);
  assert.equal(avatarHash(undefined), null);
  assert.equal(avatarHash("avatars/u1"), null, "legacy mutable key");
  assert.equal(avatarHash(`media/${H1}.enc`), null);
  assert.equal(avatarHash(`avatars/../${H1}.jpg`), null);
  assert.equal(avatarHash(`avatars/u1/${H1}.svg`), null);
  assert.equal(avatarHash(`avatars/u_1/${H1}.jpg`), null, "`_` separates file-name parts");
  assert.ok(isFileUserId("01JABCDEF-x"));
  assert.ok(!isFileUserId("a/b"));
});

test("the rendition sizes match core's", () => {
  const rust = src("../pollis-core/src/commands/avatar.rs");
  const m = /pub const AVATAR_RENDITIONS: \[u32; \d+\] = \[([^\]]+)\];/.exec(rust);
  assert.ok(m, "AVATAR_RENDITIONS in avatar.rs");
  assert.deepEqual(m[1].split(",").map((s) => Number(s.trim())), [...AVATAR_RENDITIONS]);
});

test("a view gets the smallest rendition that covers it", () => {
  assert.equal(renditionFor(40 * 3), 128);
  assert.equal(renditionFor(64 * 3), 256);
  assert.equal(renditionFor(96 * 3), 512);
  assert.equal(renditionFor(5000), 512);
});

test("rendition file names round-trip, temp files do not parse", () => {
  assert.deepEqual(parseRenditionFile(file("u1", H1, 128)), {
    userId: "u1",
    hash: H1,
    px: 128,
    name: file("u1", H1, 128),
  });
  assert.equal(parseRenditionFile(`.${file("u1", H1, 128)}.tmp`), null);
  assert.equal(parseRenditionFile("u1_short_128.jpg"), null);
  assert.equal(siblingUri(`${DIR}${file("u1", H1, 128)}`, 512), `${DIR}${file("u1", H1, 512)}`);
  assert.equal(siblingUri(`${DIR}u1_${H1}_128.png`, 256), `${DIR}u1_${H1}_256.png`);
});

interface Fake {
  deps: AvatarStoreDeps;
  files: string[];
  resolves: Array<{ userId: string; key: string; px: number }>;
  fetches: string[][];
  deleted: string[];
  warmed: string[];
  // Run the scheduled profile batch.
  flush: () => void;
  profiles: Map<string, string | null>;
  failResolve: boolean;
}

function fake(files: string[] = []): Fake {
  const f: Fake = {
    files,
    resolves: [],
    fetches: [],
    deleted: [],
    warmed: [],
    flush: () => {},
    profiles: new Map(),
    failResolve: false,
    deps: undefined as unknown as AvatarStoreDeps,
  };
  let scheduled: Array<() => void> = [];
  f.flush = () => {
    const run = scheduled;
    scheduled = [];
    run.forEach((fn) => fn());
  };
  f.deps = {
    listFiles: async () => f.files,
    fileUri: (name) => `${DIR}${name}`,
    resolve: async (userId, k, px) => {
      f.resolves.push({ userId, key: k, px });
      if (f.failResolve) {
        throw new Error("offline");
      }
      return `${DIR}${userId}_${avatarHash(k)}_${px}.jpg`;
    },
    fetchKeys: async (ids) => {
      f.fetches.push(ids);
      return ids.filter((id) => f.profiles.has(id)).map((id) => ({ id, avatar_url: f.profiles.get(id) }));
    },
    deleteFiles: (uris) => {
      f.deleted.push(...uris);
    },
    schedule: (fn) => {
      scheduled.push(fn);
    },
    warm: (uris) => {
      f.warmed.push(...uris);
    },
  };
  return f;
}

const settle = () => new Promise((r) => setImmediate(r));

test("a picture on disk draws on the first frame after launch, before any profile read", async () => {
  const f = fake([file("u1", H1, 128), file("u1", H1, 256), file("u1", H1, 512), "junk.txt"]);
  const store = createAvatarStore(f.deps);
  await store.hydrate();
  assert.equal(store.view("u1", 128), `${DIR}${file("u1", H1, 128)}`);
  assert.equal(store.view("u2", 128), null);
  // The row also asks for the key; the picture stays up meanwhile.
  store.ensure("u1", 128);
  assert.equal(store.view("u1", 128), `${DIR}${file("u1", H1, 128)}`);
});

test("a key that matches the file on disk fetches nothing", async () => {
  const f = fake([file("u1", H1, 128)]);
  const store = createAvatarStore(f.deps);
  await store.hydrate();
  store.noteKey("u1", key("u1", H1));
  store.ensure("u1", 128);
  await settle();
  assert.equal(f.resolves.length, 0);
});

test("a changed picture keeps the old one up until the new one is on disk", async () => {
  const f = fake([file("u1", H1, 128)]);
  const store = createAvatarStore(f.deps);
  await store.hydrate();
  store.noteKey("u1", key("u1", H2));
  store.ensure("u1", 128);
  assert.equal(store.view("u1", 128), `${DIR}${file("u1", H1, 128)}`, "no letter flash");
  await settle();
  assert.equal(store.view("u1", 128), `${DIR}${file("u1", H2, 128)}`);
  assert.deepEqual(f.warmed, [`${DIR}${file("u1", H2, 128)}`]);
});

test("a removed picture goes at once, files included", async () => {
  const f = fake([file("u1", H1, 128), file("u1", H1, 512)]);
  const store = createAvatarStore(f.deps);
  await store.hydrate();
  store.noteKey("u1", "");
  assert.equal(store.view("u1", 128), null);
  assert.deepEqual(new Set(f.deleted), new Set([`${DIR}${file("u1", H1, 128)}`, `${DIR}${file("u1", H1, 512)}`]));
});

test("every user a render pass does not know is looked up in ONE profile read", async () => {
  const f = fake();
  f.profiles.set("a", key("a", H1));
  f.profiles.set("b", "");
  const store = createAvatarStore(f.deps);
  for (const id of ["a", "b", "c", "a"]) {
    store.ensure(id, 128);
  }
  f.flush();
  await settle();
  assert.deepEqual(f.fetches, [["a", "b", "c"]]);
  // a has a picture: fetched on the next ensure; b and c have none.
  store.ensure("a", 128);
  store.ensure("b", 128);
  store.ensure("c", 128);
  await settle();
  assert.deepEqual(f.resolves.map((r) => r.userId), ["a"]);
  assert.equal(store.view("b", 128), null);
  // Asked once per launch, not on every render.
  store.ensure("c", 128);
  f.flush();
  assert.equal(f.fetches.length, 1);
});

test("a large roster is split into bounded profile reads", async () => {
  const f = fake();
  const store = createAvatarStore(f.deps);
  for (let i = 0; i < PROFILE_BATCH_MAX + 5; i++) {
    store.ensure(`u${i}`, 128);
  }
  f.flush();
  await settle();
  assert.deepEqual(f.fetches.map((c) => c.length), [PROFILE_BATCH_MAX, 5]);
});

test("a key a list carried beats a profile read that lands after it", async () => {
  const f = fake();
  f.profiles.set("a", "");
  const store = createAvatarStore(f.deps);
  store.ensure("a", 128);
  store.noteKey("a", key("a", H1));
  f.flush();
  await settle();
  store.ensure("a", 128);
  await settle();
  assert.equal(store.view("a", 128), `${DIR}${file("a", H1, 128)}`);
});

test("one fetch per picture at a time, and a failure waits for the next foreground", async () => {
  const f = fake();
  f.failResolve = true;
  const store = createAvatarStore(f.deps);
  store.noteKey("a", key("a", H1));
  store.ensure("a", 128);
  store.ensure("a", 128);
  await settle();
  assert.equal(f.resolves.length, 1);
  store.ensure("a", 128);
  await settle();
  assert.equal(f.resolves.length, 1, "no retry loop");
  f.failResolve = false;
  store.retryFailed();
  store.ensure("a", 128);
  await settle();
  assert.equal(f.resolves.length, 2);
  assert.ok(store.view("a", 128));
});

test("an image that will not draw is deleted and fetched once more, then left alone", async () => {
  const f = fake([file("a", H1, 128)]);
  const store = createAvatarStore(f.deps);
  await store.hydrate();
  store.noteKey("a", key("a", H1));
  const uri = store.view("a", 128)!;
  store.forget("a", uri);
  assert.deepEqual(f.deleted, [uri]);
  assert.equal(store.view("a", 128), null);
  store.ensure("a", 128);
  await settle();
  assert.equal(f.resolves.length, 1);
  store.forget("a", store.view("a", 128)!);
  store.ensure("a", 128);
  await settle();
  assert.equal(f.resolves.length, 1, "second failure stops refetching");
});

test("our own upload is drawn at once from the files core just wrote", () => {
  const f = fake();
  const store = createAvatarStore(f.deps);
  store.adopt("me", key("me", H1), `${DIR}${file("me", H1, 512)}`);
  assert.equal(store.view("me", 128), `${DIR}${file("me", H1, 128)}`);
  store.ensure("me", 128);
  assert.equal(f.resolves.length, 0);
});

test("prefetch warms what is on disk and emits once for the whole list", async () => {
  const f = fake([file("a", H1, 128), file("b", H2, 128)]);
  const store = createAvatarStore(f.deps);
  await store.hydrate();
  let emits = 0;
  store.subscribe(() => {
    emits += 1;
  });
  store.prefetch(
    [
      { userId: "a", key: key("a", H1) },
      { userId: "b", key: key("b", H2) },
    ],
    128,
  );
  assert.equal(emits, 1);
  assert.deepEqual(f.warmed, [`${DIR}${file("a", H1, 128)}`, `${DIR}${file("b", H2, 128)}`]);
});

test("nothing fetched before sign-out lands after it", async () => {
  const f = fake();
  const store = createAvatarStore(f.deps);
  store.noteKey("a", key("a", H1));
  store.ensure("a", 128);
  store.clear();
  await settle();
  assert.equal(store.view("a", 128), null);
});

test("the session teardown wipes the avatar directory", () => {
  assert.match(src("lib/session/index.ts"), /clearAvatarCache,/);
  assert.match(src("lib/avatarCache.ts"), /deleteAsync\(AVATAR_DIR/);
  assert.match(src("app/_layout.tsx"), /startAvatarCache\(\)/);
});

test("the photo is drawn from memory with no fade, and the letter only when there is none", () => {
  const s = src("components/UserAvatar.tsx");
  assert.match(s, /cachePolicy="memory"/);
  assert.match(s, /transition=\{0\}/);
  assert.match(s, /onError=\{\(\) => avatarStore\.forget\(userId, uri\)\}/);
});

test("every surface that names a person draws their picture", () => {
  for (const rel of [
    "components/direct/ConversationRow.tsx",
    "components/chat/MessageRow.tsx",
    "components/chat/Composer.tsx",
    "components/search/SearchResultRow.tsx",
    "components/search/GlobalSearchResults.tsx",
    "components/self/ProfileCard.tsx",
    "app/user/[id].tsx",
    "app/dm/info.tsx",
    "app/dm/new.tsx",
    "app/dm/requests.tsx",
    "app/conversation/info.tsx",
    "app/group/members.tsx",
    "app/group/requests.tsx",
    "app/self/blocked.tsx",
  ]) {
    const s = src(rel);
    assert.match(s, /<UserAvatar\b/, `${rel} draws UserAvatar`);
    // Group results keep their letter tile; groups have no picture.
    if (!rel.endsWith("GlobalSearchResults.tsx")) {
      assert.doesNotMatch(s, /<Avatar\b/, `${rel} still draws a letter-only Avatar`);
    }
  }
  assert.match(src("app/chat/thread.tsx"), /senderId=\{m\.sender_id\}/);
  assert.match(src("app/chat/[id].tsx"), /senderAvatarKey=\{avatarKeyById\.get\(m\.sender_id\)\}/);
});

test("a new group's seeded roster never stands in for the creator's picture", () => {
  // The seed has no avatar key. Read the real roster at once: before this, only the
  // inbox nudge refreshed it, and with that lost the creator's rows kept the letter.
  const create = src("hooks/queries/useUserGroups.ts");
  assert.match(
    create,
    /setQueryData<GroupMember\[\]>\(\s*groupInviteQueryKeys\.members\(group\.id\)[\s\S]*?\);\s*queryClient\.invalidateQueries\(\{\s*queryKey: groupInviteQueryKeys\.members\(group\.id\),/,
  );
  // And a member row without the field is "not known", never "no picture".
  assert.match(src("app/chat/[id].tsx"), /for \(const m of groupMembers \?\? \[\]\) \{\s*map\.set\(m\.user_id, m\.avatar_url\);/);
  assert.match(src("app/group/members.tsx"), /avatarKey=\{m\.avatar_url\}/);
  assert.doesNotMatch(src("app/group/members.tsx"), /avatarKey=\{m\.avatar_url \?\? null\}/);
});

test("a tagged photo reaches the screen probe until it unmounts", async () => {
  const { reportAvatarPhoto, avatarPhotosShowing } = await import("../lib/avatarRenderState.ts");
  const a = reportAvatarPhoto("row-message-1-avatar");
  const b = reportAvatarPhoto("row-message-1-avatar");
  assert.deepEqual(avatarPhotosShowing(), ["row-message-1-avatar"]);
  a();
  a();
  assert.deepEqual(avatarPhotosShowing(), ["row-message-1-avatar"], "a double withdrawal counts once");
  b();
  assert.deepEqual(avatarPhotosShowing(), []);
  assert.match(src("components/ui.tsx"), /<AvatarProbe \/>/);
});
