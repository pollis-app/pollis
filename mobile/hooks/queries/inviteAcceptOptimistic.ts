// What accepting a group invite does to the cached lists, before the accept
// has answered (#1560).
//
// `useAcceptGroupInvite` used to await `accept_group_invite` and
// `poll_mls_welcomes`, then fire the invalidations without awaiting them. The
// busy state ended, and the invite row and the group list only changed when
// those refetches came back — a visible pause with nothing happening. Now the
// invite leaves the pending list and the group joins the group lists on the
// same frame as the tap, and a failure puts every list back as it was.
//
// The rule that matters: the UI never shows NEITHER the invite nor the group.
// So the group goes into every loaded group list in the same pass that takes
// the invite out, and a rollback restores all of them together.
//
// Kept pure and free of React Query so the rules can be tested with a plain
// in-memory cache (mobile/tests/invite-accept-optimistic.test.ts). The desktop
// twin is frontend/src/hooks/queries/inviteAcceptOptimistic.ts.

/** The slice of a QueryClient this module needs. */
export interface CacheLike {
  getQueryData(key: readonly unknown[]): unknown;
  setQueryData(key: readonly unknown[], data: unknown): unknown;
}

export interface InviteLike {
  id: string;
  group_id: string;
  group_name: string;
}

/** One group list cache and the row to put into it for the accepted group. */
export interface GroupListTarget<G extends { id: string }> {
  key: readonly unknown[];
  row: (invite: InviteLike) => G;
}

/** The caches as they were before the accept, so a failure can restore them. */
export interface AcceptSnapshot {
  entries: Array<{ key: readonly unknown[]; data: unknown }>;
  /** The accepted group, whose own caches (channels, roster) the settle refreshes. */
  groupId?: string;
}

/** The pending list without one invite. Same array if it was not there. */
export function withoutInvite<I extends { id: string }>(invites: readonly I[], inviteId: string): I[] {
  if (!invites.some((i) => i.id === inviteId)) {
    return invites as I[];
  }
  return invites.filter((i) => i.id !== inviteId);
}

/**
 * A group list with the accepted group in it, appended at the end. Same array
 * if the group is already listed (a refetch beat the accept, or a second tap).
 */
export function withGroup<G extends { id: string }>(groups: readonly G[], group: G): G[] {
  if (groups.some((g) => g.id === group.id)) {
    return groups as G[];
  }
  return [...groups, group];
}

/**
 * Apply the accept to the cache: drop the invite from the pending list and put
 * the group into every loaded group list. Returns what was there before, for
 * `rollbackAcceptedInvite`. A list that was never loaded is left alone — it
 * will load with the group in it.
 */
export function applyAcceptedInvite(
  cache: CacheLike,
  opts: {
    invitesKey: readonly unknown[];
    inviteId: string;
    groupLists: ReadonlyArray<GroupListTarget<{ id: string }>>;
  },
): AcceptSnapshot {
  const snapshot: AcceptSnapshot = { entries: [] };
  const invites = cache.getQueryData(opts.invitesKey) as InviteLike[] | undefined;
  const invite = invites?.find((i) => i.id === opts.inviteId);
  if (!invites || !invite) {
    return snapshot;
  }

  for (const target of opts.groupLists) {
    const groups = cache.getQueryData(target.key) as Array<{ id: string }> | undefined;
    if (!groups) {
      continue;
    }
    const next = withGroup(groups, target.row(invite));
    if (next !== groups) {
      snapshot.entries.push({ key: target.key, data: groups });
      cache.setQueryData(target.key, next);
    }
  }

  snapshot.groupId = invite.group_id;
  snapshot.entries.push({ key: opts.invitesKey, data: invites });
  cache.setQueryData(opts.invitesKey, withoutInvite(invites, opts.inviteId));
  return snapshot;
}

/** Put every cache `applyAcceptedInvite` changed back as it was. */
export function rollbackAcceptedInvite(cache: CacheLike, snapshot: AcceptSnapshot | undefined): void {
  for (const entry of snapshot?.entries ?? []) {
    cache.setQueryData(entry.key, entry.data);
  }
}
