/*
 * The no-periodic-polling rule, as a test (#874).
 *
 * CLAUDE.md bans `setInterval` keepalives outright: freshness comes from
 * realtime events, explicit invalidation, or `with_retry` on the Rust side.
 * Two of them had survived anyway — a 2-second enrollment-status poll and a
 * 15-minute update poll — and neither was visible from any behavioural test,
 * because "a timer nobody watches fires again" leaves no trace in the UI.
 *
 * So the guard is a source scan, which is the honest shape for this rule: it
 * is a rule ABOUT the code, not about what the code renders. Every remaining
 * `setInterval` in the renderer has to be on the allowlist below with a reason,
 * which is what makes adding a new one a decision rather than an accident.
 *
 *   node --test frontend/tests/
 */

import test from "node:test";
import assert from "node:assert/strict";
import { readdirSync, readFileSync, statSync } from "node:fs";
import { join, relative } from "node:path";
import { fileURLToPath } from "node:url";

const SRC = fileURLToPath(new URL("../src", import.meta.url));

/**
 * Files permitted to call `setInterval`, each with the reason it is not a poll.
 *
 * The bar for this list: the timer must drive something already on screen and
 * touch NO network. A timer that asks anything — the DS, Turso, R2, a Tauri
 * command that does any of those — belongs on an event instead.
 */
const ALLOWED = new Map<string, string>([
  [
    "components/Auth/EnrollmentGateScreen.tsx",
    "1s countdown clock — re-renders the rendered M:SS, makes no request",
  ],
]);

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

test("no renderer module polls on a timer", () => {
  const offenders: string[] = [];
  for (const file of walk(SRC)) {
    const rel = relative(SRC, file).split("\\").join("/");
    const source = readFileSync(file, "utf8");
    // `setInterval(` in any spelling the codebase uses (`window.setInterval`,
    // bare, `globalThis.`). Comments mentioning the rule are not calls.
    const calls = source.match(/(?:^|[^.\w])(?:window\.|globalThis\.)?setInterval\s*\(/gm);
    if (!calls) {
      continue;
    }
    if (ALLOWED.has(rel)) {
      continue;
    }
    offenders.push(`${rel} (${calls.length} call${calls.length === 1 ? "" : "s"})`);
  }

  assert.deepEqual(
    offenders,
    [],
    `setInterval is banned outside the allowlist in ${import.meta.url}:\n  ${offenders.join("\n  ")}`,
  );
});

test("the enrollment gate waits on one awaited call, not a status poll", () => {
  const source = readFileSync(
    join(SRC, "components/Auth/EnrollmentGateScreen.tsx"),
    "utf8",
  );
  // The wait is a single `await_enrollment_approval` round trip that Rust
  // holds open with backoff, bounded by the request's own TTL.
  assert.match(source, /awaitEnrollmentApproval/);
  assert.doesNotMatch(
    source,
    /pollEnrollmentStatus/,
    "the 2-second status poll is back",
  );
});

test("the update checker is driven by window focus, not an interval", () => {
  const source = readFileSync(join(SRC, "services/updatePoller.ts"), "utf8");
  assert.doesNotMatch(source, /setInterval\s*\(/, "the 15-minute update poll is back");
  assert.match(source, /addEventListener\("focus"/);
  assert.match(source, /addEventListener\("visibilitychange"/);
});
