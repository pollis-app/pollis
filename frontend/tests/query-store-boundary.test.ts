/* The React Query / MobX boundary (#928, #929) as source-scan guards: no queryFn writes a store, and deleted exports stay deleted. */

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

/** Every `queryFn:` body, brace-matched so nested braces can't end it early. */
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

/** A MobX store mutation; reads are fine. */
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

  // The write still happens, keyed on the resolved value (the local voice tile needs it).
  assert.match(
    source,
    /useEffect\([\s\S]*?setUserAvatarUrl/,
    "the store must still be hydrated from the profile, now via an effect",
  );
});

/** Exports deleted by #929 and their replacements. */
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
  // The key is still an invalidation target.
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
