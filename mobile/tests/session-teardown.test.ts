/*
 * Decrypted data does not outlive the session (#1256).
 *
 * Sign-out left decrypted attachments in `pollis-media/`, export archives in
 * `pollis-export/` and message bodies in the React Query cache; a crashed
 * run left the same plaintext on disk until the next sign-out. Every path
 * that ends a session now goes through `endSession()` (lib/session), whose
 * pure half is `tearDownSession`, and the root layout sweeps leftovers once
 * per process at startup.
 *
 * Pinned here: the teardown clears every piece (and keeps going when one
 * fails), no sign-out path goes around it, and the startup sweep runs once,
 * from the root layout, never from the re-enterable boot screen.
 */

import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync, readdirSync, statSync } from "node:fs";
import { dirname, join, relative } from "node:path";
import { fileURLToPath } from "node:url";

import {
  oncePerProcess,
  sweepPlaintext,
  tearDownSession,
  type SessionTeardownDeps,
} from "../lib/session/teardown.ts";

const ROOT = join(dirname(fileURLToPath(import.meta.url)), "..");
const read = (rel: string) => readFileSync(join(ROOT, rel), "utf8");

function sourceFiles(dir: string): string[] {
  const out: string[] = [];
  for (const name of readdirSync(join(ROOT, dir))) {
    const rel = join(dir, name);
    if (statSync(join(ROOT, rel)).isDirectory()) {
      out.push(...sourceFiles(rel));
    } else if (/\.tsx?$/.test(name)) {
      out.push(rel);
    }
  }
  return out;
}

function recordingDeps(calls: string[], failing: string[] = []): SessionTeardownDeps {
  const step = (name: string) => async () => {
    calls.push(name);
    if (failing.includes(name)) {
      throw new Error(`${name} failed`);
    }
  };
  return {
    resetStore: () => {
      calls.push("resetStore");
    },
    leaveSignedInScreens: () => {
      calls.push("leaveSignedInScreens");
    },
    clearQueryCache: step("clearQueryCache"),
    clearMediaCache: step("clearMediaCache"),
    clearExportArchives: step("clearExportArchives"),
    clearEmojiCache: step("clearEmojiCache"),
  };
}

async function quietly<T>(fn: () => Promise<T>): Promise<T> {
  const warn = console.warn;
  console.warn = () => {};
  try {
    return await fn();
  } finally {
    console.warn = warn;
  }
}

test("the teardown resets the store, leaves the signed-in screens, then clears everything", async () => {
  const calls: string[] = [];
  await tearDownSession(recordingDeps(calls));
  // Screens go before the caches, so none is left mounted to refetch into
  // or re-read what is being cleared.
  assert.deepEqual(calls.slice(0, 2), ["resetStore", "leaveSignedInScreens"]);
  assert.deepEqual(new Set(calls.slice(2)), new Set([
    "clearQueryCache",
    "clearMediaCache",
    "clearExportArchives",
    "clearEmojiCache",
  ]));
});

test("one failed wipe neither stops the others nor keeps the user signed in", async () => {
  const calls: string[] = [];
  await quietly(() => tearDownSession(recordingDeps(calls, ["clearQueryCache", "clearMediaCache"])));
  assert.ok(calls.includes("resetStore"), "a failed wipe must not leave the user signed in");
  assert.ok(calls.includes("clearExportArchives"));
  assert.ok(calls.includes("clearEmojiCache"));
});

test("endSession wires the real wipes", () => {
  const src = read("lib/session/index.ts");
  for (const dep of [
    "resetStore: appStore.logout",
    "queryClient.clear()",
    "clearMediaCache,",
    "clearExportArchives,",
    "clearEmojiCache,",
    "router.dismissAll()",
    'router.replace("/(auth)/email")',
  ]) {
    assert.ok(src.includes(dep), `endSession no longer wires ${dep}`);
  }
});

test("the startup sweep clears media and export archives", async () => {
  const calls: string[] = [];
  await quietly(() =>
    sweepPlaintext({
      clearMediaCache: async () => {
        calls.push("media");
        throw new Error("disk");
      },
      clearExportArchives: async () => {
        calls.push("export");
      },
    })(),
  );
  assert.deepEqual(new Set(calls), new Set(["media", "export"]));
  assert.match(
    read("lib/session/index.ts"),
    /oncePerProcess\(\s*sweepPlaintext\(\{ clearMediaCache, clearExportArchives \}\)/,
  );
});

test("the startup sweep runs once per process, however often it is asked for", async () => {
  let runs = 0;
  const sweep = oncePerProcess(async () => {
    runs += 1;
  });
  await Promise.all([sweep(), sweep()]);
  await sweep();
  assert.equal(runs, 1, "a second sweep could wipe media out from under mounted screens");
});

test("a failed startup sweep does not block boot", async () => {
  const sweep = oncePerProcess(async () => {
    throw new Error("disk");
  });
  await quietly(() => sweep());
});

test("the root layout sweeps before rendering; the re-enterable boot screen never does", () => {
  const layout = read("app/_layout.tsx");
  assert.match(layout, /sweepStalePlaintext\(\)\.finally\(\(\) => setSweepReady\(true\)\)/);
  assert.match(layout, /if \(!loaded \|\| !bridgeReady \|\| !languageReady \|\| !sweepReady\)/);
  // app/index.tsx is re-entered mid-session (app/invite/[token].tsx routes
  // back to "/"), so a sweep there would delete media mounted screens use.
  for (const rel of ["app/index.tsx", "app/invite/[token].tsx"]) {
    assert.doesNotMatch(read(rel), /sweepStalePlaintext|clearMediaCache/, `${rel} must not sweep`);
  }
});

test("every sign-out path goes through endSession", () => {
  for (const rel of [
    "hooks/queries/useAuth.ts",
    "app/self/delete-account.tsx",
    "hooks/useInboxRealtime.ts",
  ]) {
    assert.match(read(rel), /endSession\(\)/, `${rel} no longer calls endSession()`);
  }
});

test("nothing but endSession resets the store on sign-out", () => {
  const offenders = ["app", "components", "hooks", "lib"]
    .flatMap(sourceFiles)
    .filter((rel) => relative("lib/session", rel).startsWith(".."))
    .filter((rel) => /appStore\.logout\b/.test(read(rel)));
  assert.deepEqual(
    offenders,
    [],
    "a sign-out path that calls appStore.logout() directly skips the teardown",
  );
});

test("the media viewer's named copies respect a sign-out clear", () => {
  const cache = read("lib/media/cache.ts");
  // A copy that finishes after the clear is deleted, not handed out.
  assert.match(cache, /copyAsync[\s\S]*?if \(startedIn !== generation\)[\s\S]*?deleteAsync\(dir/);
  assert.match(cache, /releaseNamedMediaUri\([\s\S]*?resolvedIn\?: number[\s\S]*?resolvedIn !== generation/);
  // Every release passes the generation it resolved in: the viewer closes
  // (and its hooks unmount) after the clear when sign-out pops it.
  for (const rel of ["app/media.tsx", "hooks/useNamedMediaUri.ts"]) {
    const src = read(rel);
    const calls = src.match(/releaseNamedMediaUri\([^;]*\);/g) ?? [];
    assert.ok(calls.length > 0, `${rel} no longer releases named copies`);
    for (const call of calls) {
      assert.match(call, /resolvedIn,?\s*\)/, `${rel}: ${call} drops the generation`);
    }
  }
});
