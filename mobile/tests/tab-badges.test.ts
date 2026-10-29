/*
 * Tab bar badges (#1561), `mobile/lib/tabBadges.ts`.
 *
 * Pending message requests count into the Direct badge and pending group
 * invites into the Groups badge, the way Slack and Discord count requests
 * into the DM badge, so you learn of one without opening the tab. These pin
 * the counting, the spoken label, and the wiring that keeps the badge live.
 */

import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { badgeText, badgeTotal, tabA11yLabel, tabBadges } from "../lib/tabBadges.ts";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const read = (p: string) => readFileSync(join(root, p), "utf8");

const base = {
  unreadCounts: { "chan-1": 2, "chan-2": 1, "dm-1": 4, "dm-elsewhere": 9 },
  channelIds: ["chan-1", "chan-2"],
  dmIds: ["dm-1"],
  requestCount: 0,
  inviteCount: 0,
};

test("unread alone: each tab sums only the ids it lists", () => {
  const b = tabBadges(base);
  assert.deepEqual(b.groups, { unread: 3, pending: 0 });
  assert.deepEqual(b.direct, { unread: 4, pending: 0 });
});

test("requests count into Direct, invites into Groups", () => {
  const b = tabBadges({ ...base, requestCount: 2, inviteCount: 1 });
  assert.equal(badgeTotal(b.direct), 6);
  assert.equal(badgeTotal(b.groups), 4);
});

test("a request with nothing unread still badges the tab", () => {
  const b = tabBadges({ ...base, unreadCounts: {}, requestCount: 1, inviteCount: 1 });
  assert.equal(badgeTotal(b.direct), 1);
  assert.equal(badgeTotal(b.groups), 1);
});

test("accepting, declining or blocking the last one clears the badge", () => {
  const b = tabBadges({ ...base, unreadCounts: {}, requestCount: 0, inviteCount: 0 });
  assert.equal(badgeTotal(b.direct), 0);
  assert.equal(badgeTotal(b.groups), 0);
  assert.equal(badgeTotal(undefined), 0);
});

test("the badge text caps at 99+", () => {
  assert.equal(badgeText(7), "7");
  assert.equal(badgeText(99), "99");
  assert.equal(badgeText(100), "99+");
});

// A stand-in `t` that shows which keys were chained, in order.
const t = (key: string, opts: Record<string, unknown>) => `${opts.label} [${key}:${opts.count}]`;

test("the spoken label says what is waiting, not just a number", () => {
  assert.equal(tabA11yLabel(t, "direct", "Direct", { unread: 0, pending: 0 }), "Direct");
  assert.equal(
    tabA11yLabel(t, "direct", "Direct", { unread: 2, pending: 0 }),
    "Direct [mobile:tabs.withUnread:2]",
  );
  assert.equal(
    tabA11yLabel(t, "direct", "Direct", { unread: 0, pending: 1 }),
    "Direct [mobile:tabs.withRequests:1]",
  );
  assert.equal(
    tabA11yLabel(t, "groups", "Groups", { unread: 3, pending: 2 }),
    "Groups [mobile:tabs.withUnread:3] [mobile:tabs.withInvites:2]",
  );
  assert.equal(tabA11yLabel(t, "self", "Self", undefined), "Self");
});

test("every locale has the request and invite labels", () => {
  for (const loc of ["en", "es", "fr", "ru", "uk", "zh", "ar"]) {
    const tabs = JSON.parse(read(`../frontend/src/i18n/locales/${loc}/mobile.json`)).tabs;
    for (const key of ["withRequests", "withInvites"]) {
      assert.ok(tabs[`${key}_other`], `${loc}: tabs.${key}_other`);
      assert.ok(tabs[`${key}_other`].includes("{{label}}"), `${loc}: ${key} keeps {{label}}`);
    }
  }
});

test("the tab bar reads the requests and invites queries", () => {
  const bar = read("components/TabBar.tsx");
  assert.match(bar, /useDMRequests\(\)/);
  assert.match(bar, /usePendingGroupInvites\(\)/);
  assert.match(bar, /requestCount: requests\.length/);
  assert.match(bar, /inviteCount: invites\.length/);
});

test("accept and block drop the request from the cache on the tap", () => {
  const dms = read("hooks/queries/useDMChannels.ts");
  const accept = dms.slice(dms.indexOf("export function useAcceptDMRequest"));
  assert.match(accept.slice(0, accept.indexOf("export function", 10)), /onMutate[\s\S]*dropDMRequests/);
  const blocks = read("hooks/queries/useBlocks.ts");
  const block = blocks.slice(blocks.indexOf("export function useBlockUser"));
  assert.match(block.slice(0, block.indexOf("export function", 10)), /onMutate[\s\S]*dropDMRequests/);
});

test("the inbox realtime refreshes requests and invites on their events", () => {
  const inbox = read("hooks/useInboxRealtime.ts");
  assert.match(inbox, /"dm_created"[\s\S]*dmQueryKeys\.all/);
  assert.match(inbox, /"membership_changed"[\s\S]*groupInviteQueryKeys\.pending/);
});
