import { useQuery, useQueries, useMutation, useQueryClient } from "@tanstack/react-query";
import { useMemo } from "react";
import { invoke } from "../../bridge";
import * as api from "../../services/api";
import type { GroupWithChannels } from "../../services/api";
import { appStore } from "../../stores/appStore";
import { useObserver } from "mobx-react-lite";
import type { Group, Channel, GroupMember } from "../../types";
import { deriveSlug } from "../../utils/urlRouting";
import {
  applyAcceptedInvite,
  rollbackAcceptedInvite,
  type AcceptSnapshot,
  type InviteLike,
} from "./inviteAcceptOptimistic";

export const groupQueryKeys = {
  all: ["groups"] as const,
  userGroups: (userId: string | null) => ["groups", "user", userId] as const,
  userGroupsWithChannels: (userId: string | null) => ["groups", "with-channels", userId] as const,
  group: (groupId: string) => ["groups", groupId] as const,
  channels: (groupId: string) => ["groups", groupId, "channels"] as const,
  members: (groupId: string) => ["groups", groupId, "members"] as const,
  pendingInvites: (userId: string | null) => ["group-invites", "pending", userId] as const,
  /** A group's outgoing pending invites (#1506). Under `group-invites` so the
   *  `membership_changed` wake-up's prefix invalidation covers it. */
  groupInvites: (groupId: string) => ["group-invites", "group", groupId] as const,
  joinRequests: (groupId: string) => ["group-join-requests", groupId] as const,
  myJoinRequest: (groupId: string | undefined, userId: string | null) =>
    ["group-join-requests", "my", groupId, userId] as const,
  inviteLinks: (groupId: string) => ["group-invite-links", groupId] as const,
  mutedChannels: (userId: string | null) => ["groups", "muted-channels", userId] as const,
};

/**
 * The channels this account muted (#1431), as a set of channel ids.
 *
 * Account-wide on the DS: push fan-out skips a muted member unless @named, and
 * `channel_mutes_changed` refetches it on the other devices. `useLiveKitRealtime`
 * mirrors it into `notify.ts` for desktop OS notifications.
 */
export function useMutedChannels() {
  const currentUser = useObserver(() => appStore.currentUser);

  return useQuery({
    queryKey: groupQueryKeys.mutedChannels(currentUser?.id ?? null),
    queryFn: async (): Promise<Set<string>> => {
      if (!currentUser) {
        throw new Error("No current user");
      }
      const ids = await invoke<string[]>("list_muted_channels", { userId: currentUser.id });
      return new Set(ids);
    },
    enabled: !!currentUser,
    staleTime: 1000 * 60,
  });
}

/**
 * Mute or unmute one channel for this account (#1431). Optimistic: the
 * sidebar dims and the toggle flips on the same frame, and a failure rolls
 * back to what was there.
 */
export function useSetChannelMuted() {
  const queryClient = useQueryClient();
  const currentUser = useObserver(() => appStore.currentUser);
  const key = groupQueryKeys.mutedChannels(currentUser?.id ?? null);

  return useMutation({
    mutationFn: async ({ channelId, muted }: { channelId: string; muted: boolean }) => {
      if (!currentUser) {
        throw new Error("No current user");
      }
      await invoke("set_channel_muted", { userId: currentUser.id, channelId, muted });
    },
    onMutate: async ({ channelId, muted }) => {
      await queryClient.cancelQueries({ queryKey: key });
      const previous = queryClient.getQueryData<Set<string>>(key);
      const next = new Set(previous ?? []);
      if (muted) {
        next.add(channelId);
      } else {
        next.delete(channelId);
      }
      queryClient.setQueryData(key, next);
      return { previous };
    },
    onError: (_err, _vars, context) => {
      queryClient.setQueryData(key, context?.previous);
    },
    onSettled: () => {
      queryClient.invalidateQueries({ queryKey: key });
    },
  });
}

export function useUserGroupsWithChannels() {
  const currentUser = useObserver(() => appStore.currentUser);

  return useQuery({
    queryKey: groupQueryKeys.userGroupsWithChannels(currentUser?.id ?? null),
    queryFn: async (): Promise<GroupWithChannels[]> => {
      if (!currentUser) {
        throw new Error("No current user");
      }
      return await api.listUserGroupsWithChannels(currentUser.id);
    },
    enabled: !!currentUser,
    staleTime: 1000 * 60,
  });
}

export function useUserGroups() {
  const currentUser = useObserver(() => appStore.currentUser);

  return useQuery({
    queryKey: groupQueryKeys.userGroups(currentUser?.id ?? null),
    queryFn: async (): Promise<Group[]> => {
      if (!currentUser) {
        throw new Error("No current user");
      }
      return await api.listUserGroups(currentUser.id);
    },
    enabled: !!currentUser,
    staleTime: 1000 * 60,
  });
}

// No `useGroupChannels` (#929): the sidebar reads `list_user_groups_with_channels`.
// `groupQueryKeys.channels` stays as an invalidation target.

export function useCreateGroup() {
  const queryClient = useQueryClient();
  const currentUser = useObserver(() => appStore.currentUser);
  const setGroups = appStore.setGroups;

  return useMutation({
    mutationFn: async ({
      name,
      description,
    }: {
      slug: string;
      name: string;
      description: string;
    }) => {
      if (!currentUser) {
        throw new Error("No current user");
      }

      return await invoke<{ id: string; name: string; description?: string; owner_id: string; created_at: string }>(
        'create_group',
        { name, description: description || null, ownerId: currentUser.id },
      );
    },
    onSuccess: (rawGroup) => {
      const ts = new Date(rawGroup.created_at).getTime();
      const newGroup: Group = {
        id: rawGroup.id,
        slug: '',
        name: rawGroup.name,
        description: rawGroup.description || '',
        created_by: rawGroup.owner_id,
        created_at: ts,
        updated_at: ts,
      };

      queryClient.invalidateQueries({
        queryKey: groupQueryKeys.userGroups(currentUser?.id ?? null),
      });

      queryClient.setQueryData<Group[]>(
        groupQueryKeys.userGroups(currentUser?.id ?? null),
        (oldGroups) => {
          const updated = [...(oldGroups || []), newGroup];
          setGroups(updated);
          return updated;
        },
      );
    },
  });
}

export function useJoinGroup() {
  const queryClient = useQueryClient();
  const currentUser = useObserver(() => appStore.currentUser);

  return useMutation({
    mutationFn: async (groupId: string) => {
      if (!currentUser) {
        throw new Error("No current user");
      }

      await invoke('invite_to_group', { groupId, userId: currentUser.id });
      return { id: groupId, slug: groupId, name: groupId, description: '', created_by: currentUser.id, created_at: 0, updated_at: 0 } as Group;
    },
    onSuccess: () => {
      queryClient.invalidateQueries({
        queryKey: groupQueryKeys.userGroups(currentUser?.id ?? null),
      });
    },
  });
}

export function useUpdateGroup() {
  const queryClient = useQueryClient();
  const currentUser = useObserver(() => appStore.currentUser);

  return useMutation({
    mutationFn: async ({
      groupId,
      name,
      description,
    }: {
      groupId: string;
      name?: string;
      description?: string | null;
    }) => {
      if (!currentUser) {
        throw new Error("No current user");
      }
      await invoke("update_group", {
        groupId,
        requesterId: currentUser.id,
        name: name ?? null,
        description: description ?? null,
        iconUrl: null,
      });
    },
    onSuccess: () => {
      queryClient.invalidateQueries({
        queryKey: groupQueryKeys.userGroups(currentUser?.id ?? null),
      });
      queryClient.invalidateQueries({
        queryKey: groupQueryKeys.userGroupsWithChannels(currentUser?.id ?? null),
      });
    },
  });
}

export function useUpdateGroupIcon() {
  const queryClient = useQueryClient();
  const currentUser = useObserver(() => appStore.currentUser);
  const setGroups = appStore.setGroups;

  return useMutation({
    mutationFn: async ({ groupId, iconUrl }: { groupId: string; iconUrl: string }) => {
      await api.updateGroupIcon(groupId, iconUrl);
      return { groupId, iconUrl };
    },
    onSuccess: ({ groupId, iconUrl }) => {
      queryClient.invalidateQueries({
        queryKey: groupQueryKeys.userGroups(currentUser?.id ?? null),
      });

      queryClient.setQueryData<Group[]>(
        groupQueryKeys.userGroups(currentUser?.id ?? null),
        (oldGroups) => {
          if (!oldGroups) {
            return oldGroups;
          }
          const updated = oldGroups.map((g) =>
            g.id === groupId ? { ...g, icon_url: iconUrl } : g,
          );
          setGroups(updated);
          return updated;
        },
      );
    },
  });
}

export function useCreateChannel() {
  const queryClient = useQueryClient();
  const currentUser = useObserver(() => appStore.currentUser);
  const setChannels = appStore.setChannels;

  return useMutation({
    mutationFn: async ({
      groupId,
      name,
      description,
    }: {
      groupId: string;
      slug: string;
      name: string;
      description: string;
    }) => {
      if (!currentUser) {
        throw new Error("No current user");
      }

      return await invoke<{ id: string; group_id: string; name: string; description?: string }>(
        'create_channel',
        { groupId, name, description: description || null, creatorId: currentUser.id },
      );
    },
    onSuccess: (rawChannel, variables) => {
      const newChannel: Channel = {
        id: rawChannel.id,
        group_id: rawChannel.group_id,
        slug: '',
        name: rawChannel.name,
        description: rawChannel.description || '',
        channel_type: 'text',
        created_by: currentUser?.id || '',
        created_at: 0,
        updated_at: 0,
      };

      queryClient.invalidateQueries({
        queryKey: groupQueryKeys.channels(variables.groupId),
      });
      queryClient.invalidateQueries({
        queryKey: groupQueryKeys.userGroupsWithChannels(currentUser?.id ?? null),
      });

      queryClient.setQueryData<Channel[]>(
        groupQueryKeys.channels(variables.groupId),
        (oldChannels) => {
          const updated = [...(oldChannels || []), newChannel];
          setChannels(variables.groupId, updated);
          return updated;
        },
      );
    },
  });
}

export function useUpdateChannel() {
  const queryClient = useQueryClient();
  const currentUser = useObserver(() => appStore.currentUser);

  return useMutation({
    mutationFn: async ({
      channelId,
      name,
      description,
    }: {
      groupId: string;
      channelId: string;
      name?: string;
      description?: string | null;
    }): Promise<Channel> => {
      if (!currentUser) {
        throw new Error("No current user");
      }
      return await invoke<Channel>("update_channel", {
        channelId,
        requesterId: currentUser.id,
        name: name ?? null,
        description: description ?? null,
      });
    },
    onSuccess: (_data, variables) => {
      queryClient.invalidateQueries({
        queryKey: groupQueryKeys.channels(variables.groupId),
      });
      queryClient.invalidateQueries({
        queryKey: groupQueryKeys.userGroupsWithChannels(currentUser?.id ?? null),
      });
    },
  });
}

export function useDeleteChannel() {
  const queryClient = useQueryClient();
  const currentUser = useObserver(() => appStore.currentUser);

  return useMutation({
    mutationFn: async ({ channelId }: { groupId: string; channelId: string }) => {
      if (!currentUser) {
        throw new Error("No current user");
      }
      await invoke('delete_channel', { channelId, requesterId: currentUser.id });
    },
    onSuccess: (_data, variables) => {
      queryClient.invalidateQueries({
        queryKey: groupQueryKeys.channels(variables.groupId),
      });
      queryClient.invalidateQueries({
        queryKey: groupQueryKeys.userGroupsWithChannels(currentUser?.id ?? null),
      });
    },
  });
}

export function useLeaveGroup() {
  const queryClient = useQueryClient();
  const currentUser = useObserver(() => appStore.currentUser);

  return useMutation({
    mutationFn: async (groupId: string) => {
      if (!currentUser) {
        throw new Error("No current user");
      }
      await invoke("leave_group", { groupId, userId: currentUser.id });
    },
    onSuccess: () => {
      queryClient.invalidateQueries({
        queryKey: groupQueryKeys.userGroups(currentUser?.id ?? null),
      });
      queryClient.invalidateQueries({
        queryKey: groupQueryKeys.userGroupsWithChannels(currentUser?.id ?? null),
      });
    },
  });
}

export type PendingInvite = {
  id: string;
  group_id: string;
  group_name: string;
  inviter_id: string;
  inviter_username?: string;
  created_at: string;
};

export type JoinRequest = {
  id: string;
  group_id: string;
  requester_id: string;
  requester_username?: string;
  status: string;
  created_at: string;
};

export function usePendingInvites() {
  const currentUser = useObserver(() => appStore.currentUser);

  return useQuery({
    queryKey: groupQueryKeys.pendingInvites(currentUser?.id ?? null),
    queryFn: async (): Promise<PendingInvite[]> => {
      if (!currentUser) {
        return [];
      }
      return await invoke<PendingInvite[]>('get_pending_invites', { userId: currentUser.id });
    },
    enabled: !!currentUser,
    staleTime: 1000 * 30,
  });
}

/**
 * Accept an invite (#1560). Optimistic: the invite leaves the pending list and
 * the group joins the sidebar on the same frame; a failure puts both back. The
 * mutation stays pending until the refetches replacing the optimistic rows land.
 */
export function useAcceptInvite() {
  const queryClient = useQueryClient();
  const currentUser = useObserver(() => appStore.currentUser);
  const userId = currentUser?.id ?? null;
  const invitesKey = groupQueryKeys.pendingInvites(userId);
  const groupsKey = groupQueryKeys.userGroups(userId);
  const withChannelsKey = groupQueryKeys.userGroupsWithChannels(userId);

  return useMutation({
    mutationFn: async (inviteId: string) => {
      if (!currentUser) {
        throw new Error('No current user');
      }
      await invoke('accept_group_invite', { inviteId, userId: currentUser.id });
      // Drain the Welcomes the inviter pre-generated. Awaited: the accept is not
      // done until the group can be opened.
      await invoke('poll_mls_welcomes', { userId: currentUser.id }).catch((err) => {
        console.warn('[mls] poll_mls_welcomes after accept:', err);
      });
    },
    onMutate: async (inviteId: string): Promise<AcceptSnapshot> => {
      // An in-flight refetch must not land on top of the optimistic rows.
      await Promise.all([
        queryClient.cancelQueries({ queryKey: invitesKey }),
        queryClient.cancelQueries({ queryKey: groupsKey }),
        queryClient.cancelQueries({ queryKey: withChannelsKey }),
      ]);
      const now = Date.now();
      const group = (invite: InviteLike): Group => ({
        id: invite.group_id,
        slug: deriveSlug(invite.group_name),
        name: invite.group_name,
        description: '',
        created_by: '',
        created_at: now,
        updated_at: now,
      });
      return applyAcceptedInvite(queryClient, {
        invitesKey,
        inviteId,
        groupLists: [
          { key: groupsKey, row: group },
          // Channels fill in on the refetch below.
          {
            key: withChannelsKey,
            row: (invite): GroupWithChannels => ({
              ...group(invite),
              channels: [],
              current_user_role: 'member',
            }),
          },
        ],
      });
    },
    onError: (_err, _inviteId, snapshot) => {
      rollbackAcceptedInvite(queryClient, snapshot);
    },
    // Returned, so the mutation stays pending until the real rows are in. The
    // group's own caches go too: a panel that opened on the optimistic row may
    // have read its channels or roster before the accept landed.
    onSettled: (_data, _err, _inviteId, snapshot) =>
      Promise.all([
        queryClient.invalidateQueries({ queryKey: invitesKey }),
        queryClient.invalidateQueries({ queryKey: groupsKey }),
        queryClient.invalidateQueries({ queryKey: withChannelsKey }),
        snapshot?.groupId
          ? queryClient.invalidateQueries({ queryKey: groupQueryKeys.group(snapshot.groupId) })
          : undefined,
      ]),
  });
}

export function useDeclineInvite() {
  const queryClient = useQueryClient();
  const currentUser = useObserver(() => appStore.currentUser);

  return useMutation({
    mutationFn: async (inviteId: string) => {
      if (!currentUser) {
        throw new Error('No current user');
      }
      await invoke('decline_group_invite', { inviteId, userId: currentUser.id });
    },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: groupQueryKeys.pendingInvites(currentUser?.id ?? null) });
    },
  });
}

export interface GroupMemberWithGroup extends GroupMember {
  group_id: string;
  group_name: string;
}

/**
 * Members of every group the current user is in, deduplicated by user_id, for
 * Cmd+K. `enabled` matters (#874): the closed Cmd+K panel calls this on every
 * start, so it passes `isOpen` to skip one roster fetch per group per launch.
 */
export function useAllGroupMembers(enabled: boolean): { members: GroupMemberWithGroup[] } {
  const { data: groups = [] } = useUserGroupsWithChannels();
  const currentUser = useObserver(() => appStore.currentUser);

  // #917: these groups are the user's own memberships, so the roster guard
  // never denies one; the id is passed because the command requires it.
  const queries = useQueries({
    queries: (enabled ? groups : []).map((g) => ({
      queryKey: groupQueryKeys.members(g.id),
      queryFn: async (): Promise<GroupMember[]> =>
        invoke<GroupMember[]>('get_group_members', { groupId: g.id, requesterId: currentUser?.id }),
      enabled: !!currentUser,
      staleTime: 1000 * 30,
      })),
  });

  return useMemo(() => {
    const seen = new Set<string>();
    const out: GroupMemberWithGroup[] = [];
    queries.forEach((q, i) => {
      const groupId = groups[i]?.id;
      const groupName = groups[i]?.name ?? "";
      if (!groupId || !q.data) {
        return;
      }
      for (const m of q.data) {
        if (seen.has(m.user_id)) {
          continue;
        }
        seen.add(m.user_id);
        out.push({ ...m, group_id: groupId, group_name: groupName });
      }
    });
    return { members: out };
    // queries is a fresh array each render; rely on length + data signatures
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [groups, queries.map((q) => q.dataUpdatedAt).join(",")]);
}

export function useGroupMembers(groupId: string | null) {
  const currentUser = useObserver(() => appStore.currentUser);

  return useQuery({
    queryKey: groupQueryKeys.members(groupId ?? ''),
    queryFn: async (): Promise<GroupMember[]> => {
      if (!groupId || !currentUser) {
        return [];
      }
      return await invoke<GroupMember[]>('get_group_members', { groupId, requesterId: currentUser.id });
    },
    enabled: !!groupId && !!currentUser,
    staleTime: 1000 * 30,
  });
}

/** A pending invite as the member page's "Invited" section lists it (#1506). */
export interface GroupInvite {
  id: string;
  invitee_id: string;
  invitee_username: string | null;
  inviter_id: string;
  inviter_username: string | null;
  created_at: string;
}

/** The group's pending invites: who was invited, by whom, and when. */
export function useGroupInvites(groupId: string | null) {
  const currentUser = useObserver(() => appStore.currentUser);

  return useQuery({
    queryKey: groupQueryKeys.groupInvites(groupId ?? ''),
    queryFn: async (): Promise<GroupInvite[]> => {
      if (!groupId || !currentUser) {
        return [];
      }
      return await invoke<GroupInvite[]>('list_group_invites', { groupId, userId: currentUser.id });
    },
    enabled: !!groupId && !!currentUser,
    staleTime: 1000 * 30,
  });
}

/**
 * Withdraw a pending invite. The DS allows the inviter and the group's admins;
 * the core then commits the invitee's devices out of the MLS tree.
 */
export function useRevokeGroupInvite() {
  const queryClient = useQueryClient();
  const currentUser = useObserver(() => appStore.currentUser);

  return useMutation({
    mutationFn: async ({ groupId, inviteId }: { groupId: string; inviteId: string }) => {
      if (!currentUser) {
        throw new Error('No current user');
      }
      await invoke('revoke_group_invite', { inviteId, groupId, userId: currentUser.id });
      return groupId;
    },
    onSuccess: (groupId) => {
      queryClient.invalidateQueries({ queryKey: groupQueryKeys.groupInvites(groupId) });
    },
  });
}

export function useSetMemberRole() {
  const queryClient = useQueryClient();
  const currentUser = useObserver(() => appStore.currentUser);

  return useMutation({
    mutationFn: async ({ groupId, userId, role }: { groupId: string; userId: string; role: 'admin' | 'member' }) => {
      if (!currentUser) {
        throw new Error('No current user');
      }
      await invoke('set_member_role', { groupId, userId, role, requesterId: currentUser.id });
      return groupId;
    },
    onSuccess: (groupId) => {
      queryClient.invalidateQueries({ queryKey: groupQueryKeys.members(groupId) });
    },
  });
}

export function useKickMember() {
  const queryClient = useQueryClient();
  const currentUser = useObserver(() => appStore.currentUser);

  return useMutation({
    mutationFn: async ({ groupId, userId }: { groupId: string; userId: string }) => {
      if (!currentUser) {
        throw new Error('No current user');
      }
      await invoke('remove_member_from_group', { groupId, userId, requesterId: currentUser.id });
      return groupId;
    },
    onSuccess: (groupId) => {
      queryClient.invalidateQueries({ queryKey: groupQueryKeys.members(groupId) });
      queryClient.invalidateQueries({ queryKey: groupQueryKeys.userGroupsWithChannels(currentUser?.id ?? null) });
    },
  });
}

export function useRequestGroupAccess() {
  const currentUser = useObserver(() => appStore.currentUser);

  return useMutation({
    mutationFn: async (groupId: string) => {
      if (!currentUser) {
        throw new Error('No current user');
      }
      await invoke('request_group_access', { groupId, requesterId: currentUser.id });
    },
  });
}

export function useMyJoinRequest(groupId: string | undefined) {
  const currentUser = useObserver(() => appStore.currentUser);

  return useQuery({
    queryKey: groupQueryKeys.myJoinRequest(groupId, currentUser?.id ?? null),
    queryFn: async (): Promise<JoinRequest | null> => {
      if (!currentUser || !groupId) {
        return null;
      }
      return await invoke<JoinRequest | null>('get_my_join_request', {
        groupId,
        requesterId: currentUser.id,
      });
    },
    enabled: !!currentUser && !!groupId,
    staleTime: 1000 * 30,
  });
}

export function useSendGroupInvite() {
  const currentUser = useObserver(() => appStore.currentUser);

  return useMutation({
    mutationFn: async ({ groupId, inviteeIdentifier }: { groupId: string; inviteeIdentifier: string }) => {
      if (!currentUser) {
        throw new Error('No current user');
      }
      await invoke('send_group_invite', { groupId, inviterId: currentUser.id, inviteeIdentifier });
    },
  });
}

export function useGroupJoinRequests(groupId: string | null) {
  const currentUser = useObserver(() => appStore.currentUser);

  return useQuery({
    queryKey: groupQueryKeys.joinRequests(groupId ?? ''),
    queryFn: async (): Promise<JoinRequest[]> => {
      if (!currentUser || !groupId) {
        return [];
      }
      return await invoke<JoinRequest[]>('get_group_join_requests', { groupId, requesterId: currentUser.id });
    },
    enabled: !!currentUser && !!groupId,
    // `join_requests_changed` invalidates this key, and `useAllPendingJoinRequests`
    // shares its entries (#874).
    staleTime: 1000 * 30,
  });
}

export function useApproveJoinRequest() {
  const queryClient = useQueryClient();
  const currentUser = useObserver(() => appStore.currentUser);

  return useMutation({
    mutationFn: async ({ requestId, groupId }: { requestId: string; groupId: string }) => {
      if (!currentUser) {
        throw new Error('No current user');
      }
      await invoke('approve_join_request', { requestId, approverId: currentUser.id });
      return groupId;
    },
    onSuccess: (groupId) => {
      queryClient.invalidateQueries({ queryKey: groupQueryKeys.joinRequests(groupId) });
    },
  });
}

export function useRejectJoinRequest() {
  const queryClient = useQueryClient();
  const currentUser = useObserver(() => appStore.currentUser);

  return useMutation({
    mutationFn: async ({ requestId, groupId }: { requestId: string; groupId: string }) => {
      if (!currentUser) {
        throw new Error('No current user');
      }
      await invoke('reject_join_request', { requestId, approverId: currentUser.id });
      return groupId;
    },
    onSuccess: (groupId) => {
      queryClient.invalidateQueries({ queryKey: groupQueryKeys.joinRequests(groupId) });
    },
  });
}

/**
 * Every pending join request across the groups the current user admins, as
 * `useQueries` over the same per-group keys as `useGroupJoinRequests` (#874):
 * each group is cached and invalidated on its own.
 */
export function useAllPendingJoinRequests() {
  const currentUser = useObserver(() => appStore.currentUser);
  const { data: groupsWithChannels } = useUserGroupsWithChannels();

  const adminGroupIds = useMemo(() => {
    if (!currentUser || !groupsWithChannels) {
      return [];
    }
    return groupsWithChannels
      .filter((g) => g.current_user_role === 'admin')
      .map((g) => g.id);
  }, [currentUser?.id, groupsWithChannels]);

  const queries = useQueries({
    queries: adminGroupIds.map((groupId) => ({
      queryKey: groupQueryKeys.joinRequests(groupId),
      queryFn: async (): Promise<JoinRequest[]> =>
        invoke<JoinRequest[]>('get_group_join_requests', {
          groupId,
          requesterId: currentUser!.id,
        }),
      enabled: !!currentUser,
      staleTime: 1000 * 30,
    })),
  });

  const signature = queries.map((q) => q.dataUpdatedAt).join(",");
  const data = useMemo(
    () => queries.flatMap((q) => q.data ?? []),
    // `queries` is a fresh array every render; the update stamps are the
    // stable signal that the underlying data actually moved.
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [signature],
  );

  return { data, isLoading: queries.some((q) => q.isLoading) };
}

/**
 * A freshly minted invite link (#847). Mirrors `CreatedInviteLink` in
 * `pollis-core/src/commands/groups/types.rs`.
 *
 * `token`/`url` arrive exactly once: the server stores only `sha256(secret)`,
 * so once this value is discarded the link can never be displayed again.
 */
export type CreatedInviteLink = {
  id: string;
  token: string;
  url: string;
  expires_at?: string | null;
  max_uses?: number | null;
};

/**
 * An existing link as shown in the management list. Mirrors
 * `InviteLinkSummary`. Deliberately has no token field — see above.
 */
export type InviteLinkSummary = {
  id: string;
  created_at: string;
  creator_username?: string | null;
  expires_at?: string | null;
  max_uses?: number | null;
  uses: number;
  revoked_at?: string | null;
  is_live: boolean;
};

/** Mirrors `RedeemedInvite`. */
export type RedeemedInvite = {
  group_id: string;
  group_name?: string | null;
};

export function useGroupInviteLinks(groupId: string | null) {
  const currentUser = useObserver(() => appStore.currentUser);

  return useQuery({
    queryKey: groupQueryKeys.inviteLinks(groupId ?? ''),
    queryFn: async (): Promise<InviteLinkSummary[]> => {
      if (!currentUser || !groupId) {
        return [];
      }
      return invoke<InviteLinkSummary[]>('list_group_invite_links', {
        groupId,
        userId: currentUser.id,
      });
    },
    enabled: !!currentUser && !!groupId,
  });
}

export function useCreateGroupInviteLink() {
  const currentUser = useObserver(() => appStore.currentUser);
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: async ({
      groupId,
      expiresInHours,
      maxUses,
    }: {
      groupId: string;
      expiresInHours: number | null;
      maxUses: number | null;
    }): Promise<CreatedInviteLink> => {
      if (!currentUser) {
        throw new Error('No current user');
      }
      return invoke<CreatedInviteLink>('create_group_invite_link', {
        groupId,
        creatorId: currentUser.id,
        expiresInHours,
        maxUses,
      });
    },
    onSuccess: (_data, variables) => {
      queryClient.invalidateQueries({
        queryKey: groupQueryKeys.inviteLinks(variables.groupId),
      });
    },
  });
}

export function useRevokeGroupInviteLink() {
  const currentUser = useObserver(() => appStore.currentUser);
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: async ({ linkId }: { linkId: string; groupId: string }) => {
      if (!currentUser) {
        throw new Error('No current user');
      }
      await invoke('revoke_group_invite_link', { linkId, userId: currentUser.id });
    },
    onSuccess: (_data, variables) => {
      queryClient.invalidateQueries({
        queryKey: groupQueryKeys.inviteLinks(variables.groupId),
      });
    },
  });
}

export function useRedeemGroupInviteLink() {
  const currentUser = useObserver(() => appStore.currentUser);
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: async ({ token }: { token: string }): Promise<RedeemedInvite> => {
      if (!currentUser) {
        throw new Error('No current user');
      }
      return invoke<RedeemedInvite>('redeem_group_invite_link', {
        token,
        userId: currentUser.id,
      });
    },
    onSuccess: () => {
      // Broad on purpose: the redeemer just crossed into a group they could not
      // see, so most group-scoped caches are stale.
      queryClient.invalidateQueries({ queryKey: groupQueryKeys.all });
    },
  });
}
