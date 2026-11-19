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
  "app/group/requests.tsx",
  "app/group/add-bot.tsx",
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

test("a loading Chip keeps its colours, spins, and takes no press", () => {
  const ui = read("components/ui.tsx");
  const chip = ui.slice(ui.indexOf("export function Chip("), ui.indexOf("export function Button("));
  assert.match(chip, /const inert = !!disabled \|\| !!loading;/);
  assert.match(chip, /usePressIn\(inert \? undefined : onPress, pressIn\)/);
  assert.match(chip, /disabled=\{inert \|\| !onPress\}/);
  assert.match(chip, /busy: !!loading/);
  assert.match(chip, /<ActivityIndicator testID=\{testID \? `\$\{testID\}-busy`/);
  // Colours follow `disabled` only, so the chip being worked on still reads as itself.
  assert.match(chip, /const off = disabled \? controlDisabled/);
});

test("join-request decisions are busy from the tap until they land (#1937)", () => {
  const src = read("app/group/requests.tsx");
  // The chip that was tapped spins; every other decision is disabled meanwhile.
  assert.match(src, /testID=\{`btn-approve-\$\{r\.id\}`\}[\s\S]*?loading=\{approve\.isPending && approve\.variables === r\.id\}/);
  assert.match(src, /testID=\{`btn-reject-\$\{r\.id\}`\}[\s\S]*?loading=\{reject\.isPending && reject\.variables === r\.id\}/);
  assert.equal((src.match(/disabled=\{submit\.busy && /g) ?? []).length, 2);
  assert.match(src, /mutation\.mutate\(requestId, \{ onSettled: submit\.end \}\)/);
  assert.doesNotMatch(src, /"…"/, "the busy state is the spinner, not a swapped label");
  // The decided row leaves the cached list in the render that ends the busy state.
  const hooks = read("hooks/queries/useGroupInvites.ts");
  assert.equal((hooks.match(/dropDecidedRequest\(queryClient, groupId, requestId\)/g) ?? []).length, 2);
  assert.match(hooks, /setQueryData<JoinRequest\[\]>\(key, \(list\) =>\s*list \? withoutInvite\(list, requestId\) : list/);
});
