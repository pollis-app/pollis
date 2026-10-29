/* Search lives at the top of Self; the third tab is a shortcut to Scan to sign in a device (#1550). */
import test from "node:test";
import assert from "node:assert/strict";
import { existsSync, readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { emitTabReselect, onTabReselect } from "../lib/tabReselect.ts";

const ROOT = join(dirname(fileURLToPath(import.meta.url)), "..");
const read = (rel: string) => readFileSync(join(ROOT, rel), "utf8");

test("there is no Search tab; the tabs are Groups, Direct and Self", () => {
  assert.equal(existsSync(join(ROOT, "app/(tabs)/search.tsx")), false);
  const layout = read("app/(tabs)/_layout.tsx");
  const screens = [...layout.matchAll(/<Tabs\.Screen name="([^"]+)"/g)].map((m) => m[1]);
  assert.deepEqual(screens, ["groups", "direct", "self"]);
});

test("the tab bar's third cell is the shield shortcut to link-device", () => {
  const bar = read("components/TabBar.tsx");
  const names = [...bar.matchAll(/^    name: "([^"]+)"|\{ name: "([^"]+)"/gm)].map((m) => m[1] ?? m[2]);
  assert.deepEqual(names, ["groups", "direct", "link-device", "self"]);
  assert.match(bar, /<Icon\.shield size=\{ICON\}/);
  assert.match(bar, /push: \{ pathname: "\/self\/link-device", params: \{ from: "tab" \} \}/);
  assert.match(bar, /router\.push\(tab\.push\)/);
  assert.doesNotMatch(bar, /state\.index === i/);
  assert.match(bar, /testID=\{`tab-\$\{tab\.name\}`\}/);
  assert.match(bar, /testID=\{`badge-\$\{tab\.name\}`\}/);
});

test("Self swaps its menu for the search results in place", () => {
  const self = read("app/(tabs)/self.tsx");
  assert.match(self, /const searching = searchFocused \|\| q\.length > 0;/);
  assert.match(self, /\{searching \? \(\s*<GlobalSearchResults q=\{q\} setQ=\{setQ\} userQuery=\{userQuery\} \/>\s*\) : \(\s*<Body/);
  assert.match(self, /testID="input-search"/);
  assert.match(self, /testID="btn-search-cancel"/);
  assert.match(self, /testID="btn-search-clear"/);
  assert.match(self, /onSubmitEditing=\{\(\) => \{\s*if \(trimmed\.length >= 2\) \{\s*setUserQuery\(trimmed\);/);
  assert.match(
    read("components/groups/GroupPanel.tsx"),
    /rootRouter\.navigate\(\{ pathname: "\/\(tabs\)\/self", params: \{ search: "1" \} \}\)/,
  );
});

test("a second tap on the focused tab reaches only that tab's listeners", () => {
  let self = 0;
  let groups = 0;
  const off = onTabReselect("self", () => {
    self += 1;
  });
  onTabReselect("groups", () => {
    groups += 1;
  });
  emitTabReselect("self");
  assert.equal(self, 1);
  assert.equal(groups, 0);
  off();
  emitTabReselect("self");
  assert.equal(self, 1);
  assert.match(read("components/TabBar.tsx"), /if \(focused\) \{\s*emitTabReselect\(tab\.name\);\s*return;/);
  assert.match(read("app/(tabs)/self.tsx"), /onTabReselect\("self", endSearch\)/);
});
