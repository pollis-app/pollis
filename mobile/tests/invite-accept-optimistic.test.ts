/* Accepting a group invite is optimistic (#1560): never NEITHER the invite nor the group. Pure rules + the hook's source. */

import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import {
  applyAcceptedInvite,
  rollbackAcceptedInvite,
  withGroup,
  withoutInvite,
  type CacheLike,
  type InviteLike,
} from "../hooks/queries/inviteAcceptOptimistic.ts";

const HOOK = fileURLToPath(new URL("../hooks/queries/useGroupInvites.ts", import.meta.url));

const INVITES_KEY = ["group-invites", "pending", "u_me"] as const;
const GROUPS_KEY = ["groups", "user", "u_me"] as const;
const WITH_CHANNELS_KEY = ["groups", "with-channels", "u_me"] as const;

class MemoryCache implements CacheLike {
  private data = new Map<string, unknown>();
  getQueryData(key: readonly unknown[]): unknown {
    return this.data.get(JSON.stringify(key));
  }
  setQueryData(key: readonly unknown[], value: unknown): unknown {
    this.data.set(JSON.stringify(key), value);
    return value;
  }
}

const invite = (id: string, groupId: string, name: string): InviteLike => ({
  id,
  group_id: groupId,
  group_name: name,
});

const groupLists = [
  { key: GROUPS_KEY, row: (i: InviteLike) => ({ id: i.group_id, name: i.group_name }) },
  {
    key: WITH_CHANNELS_KEY,
    row: (i: InviteLike) => ({ id: i.group_id, name: i.group_name, channels: [] as unknown[] }),
  },
];

function seeded() {
  const cache = new MemoryCache();
  cache.setQueryData(INVITES_KEY, [invite("inv_1", "g_new", "crew"), invite("inv_2", "g_other", "other")]);
  cache.setQueryData(GROUPS_KEY, [{ id: "g_old", name: "old" }]);
  cache.setQueryData(WITH_CHANNELS_KEY, [{ id: "g_old", name: "old", channels: [] }]);
  return cache;
}

const ids = (v: unknown) => (v as Array<{ id: string }>).map((x) => x.id);

test("accepting drops the invite and lists the group in the same pass", () => {
  const cache = seeded();
  const snapshot = applyAcceptedInvite(cache, { invitesKey: INVITES_KEY, inviteId: "inv_1", groupLists });

  assert.deepEqual(ids(cache.getQueryData(INVITES_KEY)), ["inv_2"]);
  assert.deepEqual(ids(cache.getQueryData(GROUPS_KEY)), ["g_old", "g_new"]);
  assert.deepEqual(ids(cache.getQueryData(WITH_CHANNELS_KEY)), ["g_old", "g_new"]);
  const row = (cache.getQueryData(WITH_CHANNELS_KEY) as Array<{ id: string; name: string; channels: unknown[] }>)[1];
  assert.equal(row.name, "crew");
  assert.deepEqual(row.channels, []);
  assert.equal(snapshot.groupId, "g_new");
});

test("never neither: at every point the invite or the group is listed", () => {
  const cache = seeded();
  const visible = () =>
    ids(cache.getQueryData(INVITES_KEY)).includes("inv_1") ||
    ids(cache.getQueryData(WITH_CHANNELS_KEY)).includes("g_new");

  assert.ok(visible());
  const snapshot = applyAcceptedInvite(cache, { invitesKey: INVITES_KEY, inviteId: "inv_1", groupLists });
  assert.ok(visible());
  rollbackAcceptedInvite(cache, snapshot);
  assert.ok(visible());
});

test("a failed accept restores every list exactly", () => {
  const cache = seeded();
  const before = {
    invites: cache.getQueryData(INVITES_KEY),
    groups: cache.getQueryData(GROUPS_KEY),
    withChannels: cache.getQueryData(WITH_CHANNELS_KEY),
  };
  const snapshot = applyAcceptedInvite(cache, { invitesKey: INVITES_KEY, inviteId: "inv_1", groupLists });
  rollbackAcceptedInvite(cache, snapshot);

  assert.equal(cache.getQueryData(INVITES_KEY), before.invites);
  assert.equal(cache.getQueryData(GROUPS_KEY), before.groups);
  assert.equal(cache.getQueryData(WITH_CHANNELS_KEY), before.withChannels);
});

test("a group list that was never loaded is left alone", () => {
  const cache = new MemoryCache();
  cache.setQueryData(INVITES_KEY, [invite("inv_1", "g_new", "crew")]);
  cache.setQueryData(WITH_CHANNELS_KEY, []);
  applyAcceptedInvite(cache, { invitesKey: INVITES_KEY, inviteId: "inv_1", groupLists });

  assert.equal(cache.getQueryData(GROUPS_KEY), undefined);
  assert.deepEqual(ids(cache.getQueryData(WITH_CHANNELS_KEY)), ["g_new"]);
});

test("a group already listed is not doubled", () => {
  const groups = [{ id: "g_new" }];
  assert.equal(withGroup(groups, { id: "g_new" }), groups);
});

test("an invite missing from the cache changes nothing", () => {
  const cache = seeded();
  const invites = cache.getQueryData(INVITES_KEY);
  const groups = cache.getQueryData(GROUPS_KEY);
  const snapshot = applyAcceptedInvite(cache, { invitesKey: INVITES_KEY, inviteId: "inv_gone", groupLists });

  assert.deepEqual(snapshot.entries, []);
  assert.equal(cache.getQueryData(INVITES_KEY), invites);
  assert.equal(cache.getQueryData(GROUPS_KEY), groups);
  rollbackAcceptedInvite(cache, undefined);
});

test("withoutInvite returns the same array when the invite is absent", () => {
  const invites = [invite("inv_1", "g", "n")];
  assert.equal(withoutInvite(invites, "nope"), invites);
  assert.deepEqual(withoutInvite(invites, "inv_1"), []);
});

test("useAcceptGroupInvite is optimistic, rolls back, and awaits its refetches", () => {
  const src = readFileSync(HOOK, "utf8");
  const start = src.indexOf("export function useAcceptGroupInvite()");
  const body = src.slice(start, src.indexOf("export function useDeclineGroupInvite()"));
  assert.ok(start >= 0);

  assert.match(body, /await invoke\("accept_group_invite"/);
  assert.match(body, /await invoke\("poll_mls_welcomes"/);
  assert.match(body, /onMutate: async[\s\S]*cancelQueries[\s\S]*applyAcceptedInvite\(queryClient/);
  assert.match(body, /onError: [\s\S]*rollbackAcceptedInvite\(queryClient, snapshot\)/);
  assert.match(body, /onSettled: \([^)]*snapshot\) =>\s*Promise\.all\(\[[\s\S]*invalidateQueries[\s\S]*\]\)/);
  for (const key of ["invitesKey", "groupsKey", "withChannelsKey"]) {
    assert.match(body, new RegExp(`invalidateQueries\\(\\{ queryKey: ${key} \\}\\)`));
  }
  assert.match(body, /invalidateQueries\(\{ queryKey: \[\.\.\.groupQueryKeys\.all, snapshot\.groupId\] \}\)/);
});
