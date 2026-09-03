/*
 * A `useMutation` result must not be an effect dependency (#1061).
 *
 * `useMutation` returns a fresh object on every render, and every mutation
 * state change (pending, success) re-renders the caller. An effect that lists
 * the result in its dependencies therefore re-runs on each of those renders,
 * and if the effect calls `mutate` it starts the next cycle itself. Channel
 * and DM pages did exactly this with `markConversationRead`: an unbounded
 * `mark_conversation_read` → invalidate → re-render → `mutate` loop that
 * pegged the renderer and grew the JS heap past 5 GB within minutes, while
 * showing nothing on screen. `mutate` itself is referentially stable, so the
 * shape that works is destructuring it: `const { mutate } = useX()`.
 *
 * A source scan, like `no-periodic-polling.test.ts`: the symptom is a loop
 * nobody can see, so the rule is enforced on the code's shape.
 *
 *   node --test frontend/tests/
 */

import test from "node:test";
import assert from "node:assert/strict";
import { readdirSync, readFileSync, statSync } from "node:fs";
import { join, relative } from "node:path";
import { fileURLToPath } from "node:url";

const SRC = fileURLToPath(new URL("../src", import.meta.url));

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

/** Exported hooks whose result is a `useMutation` result, directly or by return. */
function mutationHooks(files: string[]): Set<string> {
  const hooks = new Set<string>();
  const sources = files.map((f) => readFileSync(f, "utf8"));
  // Two passes so a hook that returns another mutation hook is caught after it.
  for (let pass = 0; pass < 2; pass++) {
    for (const source of sources) {
      const parts = source.split(/\n(?=export function )/);
      for (const part of parts) {
        const name = /^export function (use[A-Za-z]+)/.exec(part)?.[1];
        if (!name) {
          continue;
        }
        const returnsMutation = [...hooks].some((h) => new RegExp(`return ${h}\\b`).test(part));
        if (/\buseMutation\(/.test(part) || returnsMutation) {
          hooks.add(name);
        }
      }
    }
  }
  return hooks;
}

/** Index just past the `)` that closes the call whose `(` is at `open`. */
function closeOf(source: string, open: number): number {
  let depth = 0;
  for (let i = open; i < source.length; i++) {
    const c = source[i];
    if (c === "(" || c === "[" || c === "{") {
      depth++;
    } else if (c === ")" || c === "]" || c === "}") {
      depth--;
      if (depth === 0) {
        return i + 1;
      }
    }
  }
  return source.length;
}

/** The dependency names of every `useEffect` / `useLayoutEffect` in a file. */
function effectDeps(source: string): string[][] {
  const out: string[][] = [];
  const marker = /\buse(?:Layout)?Effect\(/g;
  let match: RegExpExecArray | null;
  while ((match = marker.exec(source)) !== null) {
    const open = match.index + match[0].length - 1;
    const call = source.slice(open, closeOf(source, open));
    const deps = /\[([^\]]*)\]\s*\)$/.exec(call.trimEnd());
    if (deps) {
      out.push(deps[1].split(",").map((d) => d.trim()).filter(Boolean));
    }
  }
  return out;
}

test("no useEffect depends on a useMutation result object", () => {
  const files = walk(SRC).filter((f) => !/\.test\.tsx?$/.test(f));
  const hooks = mutationHooks(files);
  assert.ok(hooks.size > 0, "expected to find mutation hooks under src/");

  const offenders: string[] = [];
  for (const file of files) {
    const source = readFileSync(file, "utf8");
    const bound = new Set<string>();
    for (const m of source.matchAll(/const ([A-Za-z_$][\w$]*) = (use[A-Za-z]+)\(/g)) {
      if (hooks.has(m[2])) {
        bound.add(m[1]);
      }
    }
    if (bound.size === 0) {
      continue;
    }
    for (const deps of effectDeps(source)) {
      for (const dep of deps) {
        if (bound.has(dep)) {
          offenders.push(`${relative(SRC, file)}: effect depends on '${dep}'`);
        }
      }
    }
  }

  assert.deepEqual(
    offenders,
    [],
    `A useMutation result is a new object every render, so an effect keyed on it ` +
      `re-runs after every mutation state change — and loops if it calls mutate. ` +
      `Destructure the stable \`mutate\` instead:\n  ${offenders.join("\n  ")}`,
  );
});
