// #1971: person rows prefer the display name, narrow rows stack their actions, and sign-out is a sheet.
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync, readdirSync, statSync } from "node:fs";
import { dirname, join, relative } from "node:path";
import { fileURLToPath } from "node:url";
import { personName, stackRowActions, STACK_ACTIONS_BELOW } from "../lib/personName.ts";

const LONG = "longhandle-tests-reqa-1117103000";

test("a display name wins over the handle", () => {
  assert.deepEqual(personName({ username: LONG, preferred_name: "Ada" }, "?"), {
    name: "Ada",
    isHandle: false,
  });
});

test("a blank display name falls back to the handle, marked for middle truncation", () => {
  assert.deepEqual(personName({ username: LONG, preferred_name: "  " }, "?"), {
    name: `@${LONG}`,
    isHandle: true,
  });
  assert.deepEqual(personName({ username: LONG }, "?", { at: false }), {
    name: LONG,
    isHandle: true,
  });
});

test("nothing known gives the fallback", () => {
  assert.deepEqual(personName({ username: null, preferred_name: null }, "Unknown"), {
    name: "Unknown",
    isHandle: false,
  });
});

test("phone-width rows stack their actions; tablet-width rows keep them inline", () => {
  // iPhone 18 Pro: a 402pt window less the page and row padding.
  assert.equal(stackRowActions(354), true);
  assert.equal(stackRowActions(STACK_ACTIONS_BELOW - 1), true);
  assert.equal(stackRowActions(STACK_ACTIONS_BELOW), false);
  assert.equal(stackRowActions(720), false);
});

test("large text stacks a row that would fit at the default size", () => {
  assert.equal(stackRowActions(600, 1), false);
  assert.equal(stackRowActions(600, 1.5), true);
  // Small text never un-stacks a narrow row.
  assert.equal(stackRowActions(354, 0.8), true);
});

const ROOT = join(dirname(fileURLToPath(import.meta.url)), "..");

function uiSources(): string[] {
  const out: string[] = [];
  const walk = (dir: string) => {
    for (const name of readdirSync(dir)) {
      const full = join(dir, name);
      if (statSync(full).isDirectory()) {
        walk(full);
      } else if (/\.tsx?$/.test(name)) {
        out.push(relative(ROOT, full));
      }
    }
  };
  for (const dir of ["app", "components", "hooks", "lib"]) {
    walk(join(ROOT, dir));
  }
  return out;
}

// Confirmations are SheetOverlays: a stock Alert ignores the theme (sign-out was the last).
test("no screen asks through a stock Alert", () => {
  const offenders = uiSources().filter((rel) =>
    /\bAlert\.(alert|prompt)\s*\(/.test(readFileSync(join(ROOT, rel), "utf8")),
  );
  assert.deepEqual(offenders, []);
});

test("sign-out asks in a SheetOverlay with its own confirm row", () => {
  const src = readFileSync(join(ROOT, "components/self/SignOutSheet.tsx"), "utf8");
  assert.match(src, /<SheetOverlay[\s\S]*testID="sheet-sign-out"/);
  assert.match(src, /testID="btn-confirm-sign-out"/);
  for (const screen of ["app/(tabs)/self.tsx", "app/self/security.tsx"]) {
    assert.match(readFileSync(join(ROOT, screen), "utf8"), /useSignOutSheet\(/, screen);
  }
});
