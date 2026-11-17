/* Every route has a breadcrumb past Home (#1966): the trail is walked for each `path:` in
   `src/router.tsx`, so a new route without a crumb fails here rather than showing just "Home". */

import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import { breadcrumbTrail, type TrailContext } from "../src/utils/breadcrumbTrail.ts";

const ROUTER = readFileSync(fileURLToPath(new URL("../src/router.tsx", import.meta.url)), "utf8");

const GROUP = "g1";
const CHANNEL = "c1";
const DM = "d1";

const ctx: TrailContext = {
  groups: [{ id: GROUP, name: "Acme", channels: [{ id: CHANNEL, name: "general" }] }],
  channels: { [GROUP]: [{ id: CHANNEL, name: "general" }] },
  dms: [{ id: DM, user2_identifier: "bob" }],
};

/** The key itself, so a crumb is visible without loading the catalogues. */
const t = (key: string) => key;

/** A route's path with sample ids in place of its params. */
function concrete(path: string): string {
  return path
    .replace("$groupId", GROUP)
    .replace("$channelId", CHANNEL)
    .replace("$conversationId", DM)
    .replace(/\$\w+/g, "x1");
}

const paths = Array.from(ROUTER.matchAll(/path: "([^"]+)"/g), (m) => m[1]);

test("router.tsx has routes to walk", () => {
  assert.ok(paths.length > 40, `found ${paths.length}`);
});

for (const path of paths.filter((p) => p !== "/")) {
  test(`${path} has a breadcrumb past Home`, () => {
    const trail = breadcrumbTrail(concrete(path), ctx, t);
    assert.ok(trail.length > 1, `${path} → ${trail.map((s) => s.label).join(" / ")}`);
    assert.equal(trail[0].to, "/");
  });
}

test("a channel's rename page names its group and channel", () => {
  const trail = breadcrumbTrail(`/groups/${GROUP}/channels/${CHANNEL}/rename`, ctx, t);
  assert.deepEqual(
    trail.map((s) => s.label),
    ["nav:breadcrumb.home", "nav:breadcrumb.groups", "Acme", "general", "channels:renameChannel.pageTitle"],
  );
});
