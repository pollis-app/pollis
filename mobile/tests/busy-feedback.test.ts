/*
 * Slow sign-in steps show feedback at once and cannot be submitted twice.
 *
 * Unlock and set-PIN derive a key with Argon2id and open the encrypted
 * database; verify-OTP, recovery and device approval are DS round trips with
 * key generation or wrapping. A screen that only flips `disabled` from the
 * mutation's `isPending` looks frozen for that time and lets a second tap that
 * lands before the re-render submit again. Every user-triggered submit on these
 * screens goes through `useSubmitLock` (synchronous claim + busy state) and
 * shows a spinner: `<Button loading>` or the PIN pad's `<PinBusy>`.
 */
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = join(dirname(fileURLToPath(import.meta.url)), "..");
const read = (rel: string) => readFileSync(join(ROOT, rel), "utf8");

// Screens whose submit is user-triggered and slow.
const LOCKED = [
  "app/(auth)/email.tsx",
  "app/(auth)/otp.tsx",
  "app/(auth)/pin.tsx",
  "app/(auth)/enrollment.tsx",
  "app/approve-device.tsx",
];

test("slow submits claim a synchronous submit lock", () => {
  for (const rel of LOCKED) {
    const src = read(rel);
    assert.match(src, /useSubmitLock\(\)/, `${rel} must guard its submit with useSubmitLock`);
    assert.match(src, /\.begin\(\)/, `${rel} must claim the lock before the call`);
    assert.match(src, /\.end\b/, `${rel} must release the lock when the call fails`);
  }
});

test("submit buttons spin while their call is in flight", () => {
  for (const rel of ["app/(auth)/email.tsx", "app/(auth)/otp.tsx", "app/(auth)/enrollment.tsx", "app/approve-device.tsx", "app/(auth)/link.tsx"]) {
    assert.match(read(rel), /\bloading=\{/, `${rel} must pass loading to its submit Button`);
  }
});

test("the PIN screens show a busy line under the dots once the PIN is in", () => {
  for (const rel of ["app/(auth)/pin.tsx", "app/self/link-device.tsx"]) {
    assert.match(read(rel), /<PinBusy\b/, `${rel} must render <PinBusy> while it works`);
  }
  // The PIN screen goes busy on the fourth tap itself, before the delayed
  // submit, so the spinner is on screen before the slow call starts.
  const pin = read("app/(auth)/pin.tsx");
  const begin = pin.indexOf("submit.begin()");
  const delayed = pin.indexOf("setTimeout(() => onComplete(next)");
  assert.ok(begin > 0 && delayed > begin, "pin.tsx must claim the lock before scheduling the submit");
});

test("a loading Button is disabled and announced busy", () => {
  const ui = read("components/ui.tsx");
  assert.match(ui, /const off = !!disabled \|\| !!loading;/);
  assert.match(ui, /usePressIn\(off \? undefined : onPress, pressIn\)/);
  assert.match(ui, /busy: !!loading/);
});
