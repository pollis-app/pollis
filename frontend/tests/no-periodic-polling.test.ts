/* The no-periodic-polling rule as a source scan (#874); every `setInterval` needs an allowlisted reason. */

import test from "node:test";
import assert from "node:assert/strict";
import { readdirSync, readFileSync, statSync } from "node:fs";
import { join, relative } from "node:path";
import { fileURLToPath } from "node:url";

const SRC = fileURLToPath(new URL("../src", import.meta.url));

/** Allowed `setInterval` callers: on-screen only, touching no network. */
const ALLOWED = new Map<string, string>([
  [
    "components/Auth/EnrollmentGateScreen.tsx",
    "1s countdown clock — re-renders the rendered M:SS, makes no request",
  ],
  [
    "pages/LinkDevicePage.tsx",
    "1s countdown clock for the device-link code's expiry — re-renders M:SS, makes no request (#1207)",
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
    // Any spelling (`window.`, bare, `globalThis.`); comments don't match.
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
  // One `await_enrollment_approval` call that Rust holds open.
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
