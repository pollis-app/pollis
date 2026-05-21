// Group/channel listing hooks. Mirrors `frontend/src/hooks/queries/useGroups.ts`
// for the read paths the mobile UI needs. Mutations (create_group,
// invite_to_group, etc.) come later when we add those flows.

import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { invoke } from "../../lib/native";
import { useAppStore } from "../../stores/appStore";
import type { Channel, Group } from "../../types";

export interface GroupWithChannels extends Group {
  channels: Channel[];
}

export const groupQueryKeys = {
  all: ["groups"] as const,
  userGroups: (userId: string | null) => ["groups", "user", userId] as const,
  userGroupsWithChannels: (userId: string | null) =>
    ["groups", "with-channels", userId] as const,
  channels: (groupId: string | null) => ["groups", groupId, "channels"] as const,
};

export function useUserGroups() {
  const currentUser = useAppStore((state) => state.currentUser);

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
  const currentUser = useAppStore((state) => state.currentUser);

  return useQuery({
    queryKey: groupQueryKeys.userGroupsWithChannels(currentUser?.id ?? null),
    queryFn: async (): Promise<GroupWithChannels[]> => {
      if (!currentUser) {
        throw new Error("No current user");
      }
      return await invoke<GroupWithChannels[]>("list_user_groups_with_channels", {
        userId: currentUser.id,
      });
    },
    enabled: !!currentUser,
    staleTime: 1000 * 60,
  });
}

export function useCreateGroup() {
  const queryClient = useQueryClient();
  const currentUser = useAppStore((s) => s.currentUser);

  return useMutation({
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
        createDefaultTextChannel: vars.createDefaultTextChannel ?? true,
        // Mobile drops voice — never create the default voice channel.
        createDefaultVoiceChannel: false,
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

export function useCreateChannel(groupId: string | null) {
  const queryClient = useQueryClient();
  const currentUser = useAppStore((s) => s.currentUser);

  return useMutation({
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
  return useQuery({
    queryKey: groupQueryKeys.channels(groupId),
    queryFn: async (): Promise<Channel[]> => {
      if (!groupId) {
        throw new Error("No group ID provided");
      }
      return await invoke<Channel[]>("list_group_channels", { groupId });
    },
    enabled: !!groupId,
    staleTime: 1000 * 60,
  });
}
