// #1972: copy and state fixes. Pure rules run here; the screens are pinned by source, the device tier by Maestro.
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { pickerTabs, sectionStartIndex } from "../components/emoji/pickerTabs.ts";

const read = (rel: string) => readFileSync(new URL(`../${rel}`, import.meta.url), "utf8");

test("the invite landing names the group before the press, and still joins only on it", () => {
  const src = read("app/invite/[token].tsx");
  const previewAt = src.indexOf('"preview_group_invite_link"');
  const joinAt = src.indexOf("const join");
  assert.ok(previewAt > 0, "the landing must preview the link");
  assert.ok(previewAt < joinAt, "the preview runs on arrival, before the join handler");
  assert.match(src, /mobile:invite\.confirmTitleNamed/);
  // A dead link fails in the effect's catch, with the one opaque message.
  assert.equal(src.split("redeem_group_invite_link").length - 1, 1);
});

test("approve-device with no request says so, and never prints an empty id", () => {
  const src = read("app/approve-device.tsx");
  assert.match(src, /if \(!requestId\) \{/);
  assert.match(src, /approval\.none/);
  // The id sentence is only rendered with an id to put in it.
  assert.match(src, /shortDeviceId \?[\s\S]{0,40}<Trans/);
  assert.match(src, /approval\.bodyNoId/);
});

test("the open-source count follows the filter", () => {
  const src = read("app/self/open-source/index.tsx");
  assert.match(src, /visible\.length === notices\.packages\.length/);
  assert.match(src, /openSource\.countFiltered/);
});

test("Search <group> carries the group into the search as its scope", () => {
  const panel = read("components/groups/GroupPanel.tsx");
  assert.match(panel, /params: \{ search: "1", searchGroup: groupId \}/);
  const self = read("app/(tabs)/self.tsx");
  assert.match(self, /setScopeGroupId\(params\.searchGroup \?\? null\)/);
  assert.match(self, /scopeGroupId=\{scopeGroupId\}/);
  const results = read("components/search/GlobalSearchResults.tsx");
  assert.match(results, /useSearchMessages\(trimmed, sort, scopeGroupId\)/);
  assert.match(results, /testID="chip-search-scope"/);
  const hook = read("hooks/queries/useSearch.ts");
  assert.match(hook, /groupId,\n\s*sort,/);
});

test("the emoji sheet's tabs follow its section headers, and a search has none", () => {
  const items = [
    { type: "header" as const, section: "recent", label: "Recent" },
    { type: "row" as const, section: "recent" },
    { type: "header" as const, section: "people", label: "People" },
    { type: "row" as const, section: "people" },
    { type: "row" as const, section: "people" },
    { type: "header" as const, section: "flags", label: "Flags" },
  ];
  assert.deepEqual(
    pickerTabs(items).map((tab) => tab.id),
    ["recent", "people", "flags"],
  );
  assert.equal(sectionStartIndex(items, "people"), 2);
  assert.equal(sectionStartIndex(items, "flags"), 5);
  assert.equal(sectionStartIndex(items, "gone"), -1);
  assert.deepEqual(pickerTabs([{ type: "row" as const, section: "search" }]), []);
  // The first sheet gets them too: the tab row is not behind any "opened before" state.
  const sheet = read("components/emoji/EmojiPickerSheet.tsx");
  assert.match(sheet, /tabs\.length > 0 \?/);
  assert.match(sheet, /testID=\{`emoji-category-\$\{tab\.id\}`\}/);
});
