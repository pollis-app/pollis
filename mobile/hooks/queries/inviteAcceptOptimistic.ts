// Accepting a group invite, applied to the cached lists before the accept answers (#1560): never show NEITHER the invite nor the group. Pure.

export interface CacheLike {
  getQueryData(key: readonly unknown[]): unknown;
  setQueryData(key: readonly unknown[], data: unknown): unknown;
}

export interface InviteLike {
  id: string;
  group_id: string;
  group_name: string;
}

export interface GroupListTarget<G extends { id: string }> {
  key: readonly unknown[];
  row: (invite: InviteLike) => G;
}

export interface AcceptSnapshot {
  entries: Array<{ key: readonly unknown[]; data: unknown }>;
  /** The accepted group, whose own caches the settle refreshes. */
  groupId?: string;
}

export function withoutInvite<I extends { id: string }>(invites: readonly I[], inviteId: string): I[] {
  if (!invites.some((i) => i.id === inviteId)) {
    return invites as I[];
  }
  return invites.filter((i) => i.id !== inviteId);
}

/** Same array if the group is already listed. */
export function withGroup<G extends { id: string }>(groups: readonly G[], group: G): G[] {
  if (groups.some((g) => g.id === group.id)) {
    return groups as G[];
  }
  return [...groups, group];
}

/** Returns the previous state for `rollbackAcceptedInvite`; unloaded lists are left alone. */
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

export function rollbackAcceptedInvite(cache: CacheLike, snapshot: AcceptSnapshot | undefined): void {
  for (const entry of snapshot?.entries ?? []) {
    cache.setQueryData(entry.key, entry.data);
  }
}
