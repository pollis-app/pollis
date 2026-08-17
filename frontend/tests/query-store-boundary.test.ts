/*
 * The React Query / MobX boundary, as tests (#928, #929).
 *
 * CLAUDE.md splits renderer state cleanly: React Query hooks own remote data,
 * MobX singletons hold UI state only. `useUserProfile` had blurred it in the
 * hardest direction to debug — its `queryFn` called `appStore.setUserAvatarUrl`
 * — and the residue of several dead code paths was still exported alongside.
 *
 * Both are GUARDS, and deliberately source scans rather than behavioural
 * specs, for the same reason `no-periodic-polling.test.ts` is: they are rules
 * ABOUT the code. "A query function must not write observable state" describes
 * a shape, and its symptom — a background refetch nobody asked for mutating a
 * store, out of order with render — is a race that no deterministic test can
 * reliably provoke. "This export has no callers" is not observable at runtime
 * at all. The behaviour that the store still gets hydrated (the reason the
 * write existed) is covered by the app: `VoiceSessionManager` reads
 * `userAvatarUrl` when it builds the local voice tile.
 *
 *   node --test frontend/tests/
 */

import test from "node:test";
import assert from "node:assert/strict";
import { readdirSync, readFileSync, statSync } from "node:fs";
import { join, relative } from "node:path";
import { fileURLToPath } from "node:url";

const SRC = fileURLToPath(new URL("../src", import.meta.url));
const QUERIES = join(SRC, "hooks/queries");

function walk(dir: string): string[] {
  const out: string[] = [];
  for (const entry of readdirSync(dir)) {
    const full = join(dir, entry);
    if (statSync(full).isDirectory()) {
      out.push(...walk(full));
    } else if (/\.(ts|tsx)$/.test(entry)) {
      out.push(full);
    }
  }
  return out;
}

/**
 * The body of every `queryFn:` in a file, as source text.
 *
 * Brace-matched from the `{` that opens the function rather than regexed to a
 * closing line, so a nested object or an inline arrow inside the body cannot
 * end the slice early and hide a write past it.
 */
function queryFnBodies(source: string): string[] {
  const bodies: string[] = [];
  const marker = /queryFn\s*:/g;
  let match: RegExpExecArray | null;
  while ((match = marker.exec(source)) !== null) {
    const open = source.indexOf("{", match.index);
    if (open === -1) {
      continue;
    }
    let depth = 0;
    let end = open;
    for (let i = open; i < source.length; i++) {
      if (source[i] === "{") {
        depth++;
      } else if (source[i] === "}") {
        depth--;
        if (depth === 0) {
          end = i;
          break;
        }
      }
    }
    bodies.push(source.slice(open, end + 1));
  }
  return bodies;
}

/**
 * A write to a MobX singleton: `<something>Store.setX(` / `.clearX(` etc.
 *
 * Reads (`appStore.currentUser`) are fine and common — a query function is
 * allowed to depend on who is signed in. Only mutation is banned.
 */
const STORE_WRITE = /\b\w*[sS]tore\.(set|clear|reset|add|remove|toggle|update|push)\w*\s*\(/;

test("GUARD: no queryFn writes to a MobX store (#928)", () => {
  const offenders: string[] = [];
  for (const file of walk(QUERIES)) {
    const rel = relative(SRC, file).split("\\").join("/");
    for (const body of queryFnBodies(readFileSync(file, "utf8"))) {
      const hit = body.match(STORE_WRITE);
      if (hit) {
        offenders.push(`${rel} — ${hit[0]}`);
      }
    }
  }

  assert.deepEqual(
    offenders,
    [],
    "A query function is not an effect: React Query runs it on refetch, on retry " +
      "and on cache-restore, with no ordering guarantee against render, and skips " +
      "it entirely on a cache hit. Store writes derived from query data belong in " +
      "a `useEffect` keyed on the result (see `useUserProfile`), or the consumer " +
      "should read the query directly.\n  " +
      offenders.join("\n  "),
  );
});

test("useUserProfile hydrates the store from an effect, not its queryFn (#928)", () => {
  const source = readFileSync(join(QUERIES, "useUserProfile.ts"), "utf8");

  const bodies = queryFnBodies(source);
  assert.ok(bodies.length > 0, "useUserProfile must still define query functions");
  for (const body of bodies) {
    assert.doesNotMatch(
      body,
      /setUserAvatarUrl/,
      "the avatar hydration is back inside a queryFn (#928)",
    );
  }

  // …and it still happens, keyed on the resolved value. Dropping the write
  // altogether would blank the local participant's avatar in a voice call,
  // which is the bug the original line existed to fix.
  assert.match(
    source,
    /useEffect\([\s\S]*?setUserAvatarUrl/,
    "the store must still be hydrated from the profile, now via an effect",
  );
});

/**
 * Exports deleted by #929, each with what replaced it. A name coming back
 * means either a real caller appeared (delete the entry) or the residue was
 * reintroduced (don't).
 */
const DELETED_API_EXPORTS = new Map<string, string>([
  ["uploadFile", "services/r2-upload.ts owns uploads"],
  ["downloadFile", "services/r2-upload.ts is the only caller of download_file"],
  ["authenticateWithClerk", "auth is requestOTP / verifyOTP"],
  ["cancelAuth", "a no-op stub with no callers"],
  ["getServiceUserData", "getUserProfile"],
  ["updateServiceUserData", "updateUserProfile"],
  ["updateServiceUserAvatar", "updateUserProfile"],
]);

test("GUARD: the Clerk-era and unused api.ts exports stay deleted (#929)", () => {
  const source = readFileSync(join(SRC, "services/api.ts"), "utf8");
  const back: string[] = [];
  for (const [name, replacement] of DELETED_API_EXPORTS) {
    if (new RegExp(`export\\s+(async\\s+)?function\\s+${name}\\b`).test(source)) {
      back.push(`${name} (use: ${replacement})`);
    }
  }
  assert.deepEqual(back, [], `dead surface reintroduced in services/api.ts:\n  ${back.join("\n  ")}`);
});

test("GUARD: the dead per-group channels hook stays deleted (#929)", () => {
  const source = readFileSync(join(QUERIES, "useGroups.ts"), "utf8");
  assert.doesNotMatch(
    source,
    /export\s+function\s+useGroupChannels\b/,
    "`useGroupChannels` had zero callers — the sidebar reads " +
      "`list_user_groups_with_channels` (#929)",
  );
  // The cache KEY is not dead: channel create / rename / delete and
  // `membership_changed` all invalidate it.
  assert.match(source, /channels:\s*\(/, "groupQueryKeys.channels is still an invalidation target");
});

test("GUARD: the settings phone field leaves nothing behind (#929)", () => {
  const page = readFileSync(join(SRC, "pages/SettingsPage.tsx"), "utf8");
  assert.doesNotMatch(
    page,
    /settings-phone-input/,
    "a testid for a control the user cannot see lets a test pass against nothing (#929)",
  );
  assert.doesNotMatch(
    page,
    /\bphone\b/,
    "the phone field was commented out while its state and save wiring stayed live (#929)",
  );

  // And the mutation no longer carries an argument nothing can supply.
  const profile = readFileSync(join(QUERIES, "useUserProfile.ts"), "utf8");
  const updateProfile = profile.slice(profile.indexOf("export function useUpdateProfile"));
  assert.doesNotMatch(
    updateProfile.slice(0, updateProfile.indexOf("export function", 1)),
    /phone/,
    "useUpdateProfile still takes a phone the renderer has no way to set (#929)",
  );
});
