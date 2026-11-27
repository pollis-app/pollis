/* A finished create/edit flow leaves no step of itself behind Back (#2129): each exit pops or replaces, never pushes. */
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = join(dirname(fileURLToPath(import.meta.url)), "..");
const read = (rel: string) => readFileSync(join(ROOT, rel), "utf8");

// The body of the first `onSuccess` handler after `anchor` (up to the next blank line).
function successAfter(src: string, anchor: string): string {
  const at = src.indexOf(anchor);
  assert.ok(at >= 0, `missing ${anchor}`);
  const start = src.indexOf("onSuccess", at);
  assert.ok(start >= 0, `no onSuccess after ${anchor}`);
  return src.slice(start, src.indexOf("\n\n", start));
}

test("exitToTab pops to the tabs on a phone instead of replacing the top page", () => {
  const src = read("components/pane/paneContext.ts");
  const body = src.slice(src.indexOf("const exitToTab"), src.indexOf("return useMemo<Nav>"));
  assert.match(body, /router\.dismissTo\(`\/\(tabs\)\/\$\{tab\}`/);
  assert.doesNotMatch(body, /router\.(replace|push)\(/);
});

test("the OTP step is replaced by the step after it, so Back never reopens a used code", () => {
  const body = successAfter(read("app/(auth)/otp.tsx"), "verifyOtp.mutate(");
  assert.match(body, /router\.replace\("\/\(auth\)\/enrollment"\)/);
  assert.match(body, /router\.replace\("\/\(auth\)\/pin"\)/);
  assert.doesNotMatch(body, /router\.push\(/);
});

test("create group, start a DM, join by invite and add a bot leave by dismiss, replace or back", () => {
  const cases: [string, string, RegExp][] = [
    ["app/group/new.tsx", "createGroup.mutate(", /router\.dismissTo\("\/\(tabs\)\/groups"\)/],
    ["app/dm/new.tsx", "", /openConversation\([^)]*\{ replace: true \}\)/],
    ["app/invite/[token].tsx", "", /openGroup\([^)]*\{ replace: true \}\)/],
    ["app/group/invite.tsx", "sendInvite.mutate(", /router\.back\(\)/],
  ];
  for (const [rel, anchor, expected] of cases) {
    const src = read(rel);
    const body = anchor === "" ? src : successAfter(src, anchor);
    assert.match(body, expected, `${rel} must leave its finished form by ${expected}`);
    if (anchor !== "") {
      assert.doesNotMatch(body, /router\.push\(/, `${rel} must not push from a finished form`);
    }
  }

  const addBot = read("app/group/add-bot.tsx");
  const created = addBot.slice(addBot.indexOf("onSuccess: (created)"));
  assert.match(created.slice(0, 300), /router\.replace\(\{/);
});
