/* Slow sign-in steps show feedback at once and cannot be submitted twice (`useSubmitLock` + a spinner). */
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = join(dirname(fileURLToPath(import.meta.url)), "..");
const read = (rel: string) => readFileSync(join(ROOT, rel), "utf8");

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

test("once the PIN is in, a loader replaces the keypad", () => {
  // Sign-in / unlock: the launch loader (the boot screen's glyph + spinner) takes the whole screen.
  const pinSrc = read("app/(auth)/pin.tsx");
  assert.match(pinSrc, /<LaunchScreen\b/);
  assert.match(pinSrc, /testID=\{view === "submitting" \? "pin-busy"/);
  assert.match(read("app/index.tsx"), /<LaunchScreen\b/, "the boot screen and the PIN loader are one component");
  assert.doesNotMatch(pinSrc, /<PinBusy\b/);
  // Scan to sign in: the busy line stands where the keypad was; the keypad is not drawn meanwhile.
  const link = read("app/self/link-device.tsx");
  assert.match(link, /\{busy \? \(\s*<View[^>]*>\s*<PinBusy label=\{t\("security\.verifying"\)\} \/>\s*<\/View>\s*\) : \(\s*<PinKeypad/);
  // Busy on the fourth tap itself, before the slow call starts.
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
