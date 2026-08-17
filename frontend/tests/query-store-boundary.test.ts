/*
 * The React Query / MobX boundary, as a test (#928).
 *
 * CLAUDE.md splits renderer state cleanly: React Query hooks own remote data,
 * MobX singletons hold UI state only. `useUserProfile` had blurred it in the
 * hardest direction to debug — its `queryFn` called `appStore.setUserAvatarUrl`.
 *
 * A GUARD, and deliberately a source scan rather than a behavioural spec, for
 * the same reason `no-periodic-polling.test.ts` is: it is a rule ABOUT the
 * code. "A query function must not write observable state" describes a shape,
 * and its symptom — a background refetch nobody asked for mutating a store,
 * out of order with render — is a race no deterministic test can reliably
 * provoke. The behaviour that the store still gets hydrated (the reason the
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
