/* Who reacted with what: the tabs and rows the long-press reaction sheet draws. */

import test from "node:test";
import assert from "node:assert/strict";

import {
  ALL_TAB_KEY,
  buildReactionBreakdown,
  mergeProfiles,
  reactorIds,
  selectTab,
  summarizeNames,
  type ReactorProfile,
} from "../src/utils/reactionBreakdown.ts";

const ME = "u-me";
const ANN = "u-ann";
const BO = "u-bo";
const GONE = "u-gone";

const profiles = new Map<string, ReactorProfile>([
  [ME, { username: "me_handle", preferred_name: "Me Person", avatar_url: null }],
  [ANN, { username: "ann", avatar_url: "avatars/ann.jpg" }],
  [BO, { username: "bo", preferred_name: "Bo" }],
]);

const reactions = [
  { emoji: "👍", user_ids: [BO, ME, ANN] },
  { emoji: "🎉", user_ids: [ANN] },
];

test("tabs follow the order the pills sit on the message, with All first", () => {
  const tabs = buildReactionBreakdown(reactions, ME, profiles);
  assert.deepEqual(
    tabs.map((t) => [t.key, t.count]),
    [
      [ALL_TAB_KEY, 4],
      ["👍", 3],
      ["🎉", 1],
    ],
  );
});

test("a single emoji gets no All tab", () => {
  const tabs = buildReactionBreakdown([reactions[0]], ME, profiles);
  assert.deepEqual(tabs.map((t) => t.key), ["👍"]);
});

test("people keep reaction order (oldest first), self included in place", () => {
  const [, thumbs] = buildReactionBreakdown(reactions, ME, profiles);
  assert.deepEqual(thumbs.people.map((p) => p.userId), [BO, ME, ANN]);
  const me = thumbs.people[1];
  assert.equal(me.isSelf, true);
  assert.equal(me.name, "Me Person");
  assert.equal(me.handle, "me_handle");
  assert.equal(thumbs.people[0].isSelf, false);
});

test("All lists each person once, with every emoji they used", () => {
  const [all] = buildReactionBreakdown(reactions, ME, profiles);
  assert.deepEqual(all.people.map((p) => p.userId), [BO, ME, ANN]);
  assert.deepEqual(all.people.find((p) => p.userId === ANN)?.emojis, ["👍", "🎉"]);
});

test("a display name equal to the username shows no separate handle", () => {
  const [, thumbs] = buildReactionBreakdown(reactions, ME, profiles);
  const bo = thumbs.people.find((p) => p.userId === BO);
  assert.equal(bo?.name, "Bo");
  assert.equal(bo?.handle, "bo");
  const ann = thumbs.people.find((p) => p.userId === ANN);
  assert.equal(ann?.name, "ann");
  assert.equal(ann?.handle, null);
  assert.equal(ann?.avatarKey, "avatars/ann.jpg");
});

test("an unknown user has no name and no avatar key, never the raw id", () => {
  const tabs = buildReactionBreakdown([{ emoji: "👍", user_ids: [GONE] }], ME, profiles);
  const gone = tabs[0].people[0];
  assert.equal(gone.name, null);
  assert.equal(gone.handle, null);
  assert.equal(gone.avatarKey, undefined);
  assert.equal(gone.isSelf, false);
});

test("blank profile fields count as unknown", () => {
  const blank = new Map<string, ReactorProfile>([[GONE, { username: "  ", preferred_name: "" }]]);
  const tabs = buildReactionBreakdown([{ emoji: "👍", user_ids: [GONE] }], ME, blank);
  assert.equal(tabs[0].people[0].name, null);
});

test("a duplicated id in one emoji is listed once", () => {
  const tabs = buildReactionBreakdown([{ emoji: "👍", user_ids: [ANN, ANN] }], ME, profiles);
  assert.equal(tabs[0].count, 1);
  assert.equal(tabs[0].people.length, 1);
});

test("empty reactions make no tabs", () => {
  assert.deepEqual(buildReactionBreakdown([], ME, profiles), []);
  assert.deepEqual(buildReactionBreakdown([{ emoji: "👍", user_ids: [] }], ME, profiles), []);
});

test("no signed-in user: nobody is self", () => {
  const tabs = buildReactionBreakdown(reactions, null, profiles);
  assert.ok(tabs.every((t) => t.people.every((p) => !p.isSelf)));
});

test("the long-pressed emoji's tab is selected, falling back to the first emoji", () => {
  const tabs = buildReactionBreakdown(reactions, ME, profiles);
  assert.equal(selectTab(tabs, "🎉")?.key, "🎉");
  assert.equal(selectTab(tabs, ALL_TAB_KEY)?.key, ALL_TAB_KEY);
  // The emoji was taken back while the sheet was open.
  assert.equal(selectTab(tabs, "😂")?.key, "👍");
  assert.equal(selectTab(tabs, null)?.key, "👍");
  assert.equal(selectTab([], "👍"), null);
});

test("reactorIds lists everyone once, in tab then reaction order", () => {
  assert.deepEqual(reactorIds(reactions), [BO, ME, ANN]);
});

test("mergeProfiles: later sources win, but never with an empty value", () => {
  const roster = new Map<string, ReactorProfile>([[ANN, { username: "ann", avatar_url: "a1" }]]);
  const fetched = new Map<string, ReactorProfile>([
    [ANN, { username: "", preferred_name: "Ann A", avatar_url: "a2" }],
    [BO, { username: "bo" }],
  ]);
  const merged = mergeProfiles(roster, undefined, fetched);
  assert.deepEqual(merged.get(ANN), { username: "ann", preferred_name: "Ann A", avatar_url: "a2" });
  assert.equal(merged.get(BO)?.username, "bo");
  assert.equal(merged.get(BO)?.avatar_url, undefined);
});

test("summarizeNames names the first few and counts the rest", () => {
  const [, thumbs] = buildReactionBreakdown(reactions, ME, profiles);
  const label = (p: { name: string | null }) => p.name ?? "?";
  assert.deepEqual(summarizeNames(thumbs.people, label, 2), { names: ["Bo", "Me Person"], others: 1 });
  assert.deepEqual(summarizeNames(thumbs.people, label), { names: ["Bo", "Me Person", "ann"], others: 0 });
});

test("mobile's copy is the same module", async () => {
  const { readFileSync } = await import("node:fs");
  const body = (path: string) =>
    readFileSync(new URL(path, import.meta.url), "utf8").split("\n").slice(2).join("\n");
  assert.equal(
    body("../../mobile/lib/reactionBreakdown.ts"),
    body("../src/utils/reactionBreakdown.ts"),
  );
});
