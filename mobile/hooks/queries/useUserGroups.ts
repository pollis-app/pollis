// Group/channel listing hooks. Mirrors `frontend/src/hooks/queries/useGroups.ts`
// for the read paths the mobile UI needs. Mutations (create_group,
// invite_to_group, etc.) come later when we add those flows.

import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { invoke } from "../../lib/native";
import { appStore } from "../../stores/appStore";
import { useObserver } from "mobx-react-lite";
import type { Channel, Group } from "../../types";
import { groupInviteQueryKeys, type GroupMember } from "./useGroupInvites";

export interface GroupWithChannels extends Group {
  channels: Channel[];
  /** The viewer's role in this group ("admin" | "member"), from the core's
   *  `GroupWithChannels.current_user_role`. */
  current_user_role?: string;
  /** The group's creator, as the core's `GroupWithChannels.owner_id` names
   *  it. Never carries the "new" member tag (#1506). */
  owner_id?: string;
}

export const groupQueryKeys = {
  all: ["groups"] as const,
  userGroups: (userId: string | null) => ["groups", "user", userId] as const,
  userGroupsWithChannels: (userId: string | null) =>
    ["groups", "with-channels", userId] as const,
  channels: (groupId: string | null) => ["groups", groupId, "channels"] as const,
  mutedChannels: (userId: string | null) => ["groups", "muted-channels", userId] as const,
};

/**
 * The channels this account muted (#1431), as a set of channel ids. Mirrors
 * desktop's `useMutedChannels`.
 *
 * Stored on the DS for the whole account: the push fan-out reads it to skip a
 * muted member unless they are @named, so a muted channel's chatter and @all
 * never reach this phone as a push — backgrounded or foreground. Every device
 * reads the same row; the DS nudges the account's other devices
 * (`channel_mutes_changed`, handled in `useInboxRealtime`) to refetch.
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
 * switch and the dimmed row flip on the same frame, and a failure rolls back.
 */
export function useSetChannelMuted() {
  const queryClient = useQueryClient();
  const currentUser = useObserver(() => appStore.currentUser);
  const key = groupQueryKeys.mutedChannels(currentUser?.id ?? null);

  return useMutation({
    // Tactile feedback (#1286): the ToggleRow ticks on flip; only a failure plays here.
    meta: { haptic: {} },
    mutationFn: async (vars: { channelId: string; muted: boolean }) => {
      if (!currentUser) {
        throw new Error("No current user");
      }
      await invoke("set_channel_muted", {
        userId: currentUser.id,
        channelId: vars.channelId,
        muted: vars.muted,
      });
    },
    onMutate: async (vars) => {
      await queryClient.cancelQueries({ queryKey: key });
      const previous = queryClient.getQueryData<Set<string>>(key);
      const next = new Set(previous ?? []);
      if (vars.muted) {
        next.add(vars.channelId);
      } else {
        next.delete(vars.channelId);
      }
      queryClient.setQueryData(key, next);
      return { previous };
    },
    onError: (_err, _vars, context) => {
      queryClient.setQueryData(key, context?.previous);
    },
    onSettled: () => {
      void queryClient.invalidateQueries({ queryKey: key });
    },
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
      return await invoke<Group[]>("list_user_groups", {
        userId: currentUser.id,
      });
    },
    enabled: !!currentUser,
    staleTime: 1000 * 60,
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
      const groups = await invoke<GroupWithChannels[]>(
        "list_user_groups_with_channels",
        { userId: currentUser.id },
      );
      // VOICE-CHANNEL FILTER: mobile has no voice UI yet (#185), so a voice
      // channel can't be opened or used — drop them here so they never render
      // in any channel list. This is the single source for that filtering on
      // the groups-tab path; the per-group path filters in useGroupChannels.
      return groups.map((g) => ({
        ...g,
        channels: g.channels.filter((c) => c.channel_type !== "voice"),
      }));
    },
    enabled: !!currentUser,
    staleTime: 1000 * 60,
  });
}

export function useCreateGroup() {
  const queryClient = useQueryClient();
  const currentUser = useObserver(() => appStore.currentUser);

  return useMutation({
    // Tactile feedback (#1286): lib/queryClient.ts plays it.
    meta: { haptic: { success: "success" } },
    mutationFn: async (vars: {
      name: string;
      description?: string;
      createDefaultTextChannel?: boolean;
    }) => {
      if (!currentUser) {
        throw new Error("No current user");
      }
      return await invoke<Group>("create_group", {
        name: vars.name,
        description: vars.description ?? null,
        ownerId: currentUser.id,
        createDefaultTextChannel: vars.createDefaultTextChannel ?? false,
        // Mobile drops voice — never create the default voice channel.
        createDefaultVoiceChannel: false,
      });
    },
    onSuccess: (group) => {
      queryClient.invalidateQueries({
        queryKey: groupQueryKeys.userGroups(currentUser?.id ?? null),
      });
      queryClient.invalidateQueries({
        queryKey: groupQueryKeys.userGroupsWithChannels(currentUser?.id ?? null),
      });
      // The app lands on the new group's panel straight away. Its roster read
      // can lag the create, so seed it with the one member it is known to
      // have — the creator, as admin (core inserts that row with the group) —
      // and the panel shows "1 member" on landing instead of no count. The
      // seed is fresh for the query's staleTime; later reads replace it.
      if (currentUser) {
        const creator: GroupMember = {
          user_id: currentUser.id,
          username: currentUser.username,
          role: "admin",
          joined_at: new Date().toISOString(),
        };
        queryClient.setQueryData<GroupMember[]>(
          groupInviteQueryKeys.members(group.id),
          (cached) => (cached && cached.length > 0 ? cached : [creator]),
        );
      }
      // The channel list (#General, when opted in) is read on landing too.
      queryClient.invalidateQueries({
        queryKey: groupQueryKeys.channels(group.id),
      });
    },
  });
}

export function useCreateChannel(groupId: string | null) {
  const queryClient = useQueryClient();
  const currentUser = useObserver(() => appStore.currentUser);

  return useMutation({
    // Tactile feedback (#1286): lib/queryClient.ts plays it.
    meta: { haptic: { success: "success" } },
    mutationFn: async (vars: { name: string; description?: string }) => {
      if (!currentUser || !groupId) {
        throw new Error("No active group");
      }
      return await invoke<Channel>("create_channel", {
        groupId,
        name: vars.name,
        description: vars.description ?? null,
        channelType: "text",
        creatorId: currentUser.id,
      });
    },
    onSuccess: () => {
      queryClient.invalidateQueries({
        queryKey: groupQueryKeys.channels(groupId),
      });
      queryClient.invalidateQueries({
        queryKey: groupQueryKeys.userGroupsWithChannels(currentUser?.id ?? null),
      });
    },
  });
}

export function useGroupChannels(groupId: string | null) {
  const currentUser = useObserver(() => appStore.currentUser);

  return useQuery({
    queryKey: groupQueryKeys.channels(groupId),
    queryFn: async (): Promise<Channel[]> => {
      if (!groupId || !currentUser) {
        throw new Error("No group ID provided");
      }
      const channels = await invoke<Channel[]>("list_group_channels", {
        groupId,
        requesterId: currentUser.id,
      });
      // VOICE-CHANNEL FILTER: mobile has no voice UI yet (#185) — never surface
      // voice channels in the list. Mirrors the filter in
      // useUserGroupsWithChannels.
      return channels.filter((c) => c.channel_type !== "voice");
    },
    enabled: !!groupId && !!currentUser,
    staleTime: 1000 * 60,
  });
}
