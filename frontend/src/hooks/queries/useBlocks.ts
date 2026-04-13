import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { useAppStore } from "../../stores/appStore";
import type { BlockedUser, DmChannel } from "../../types";

export const blocksQueryKeys = {
  dmRequests: (userId: string | null) => ["dmRequests", userId] as const,
  blockedUsers: (userId: string | null) => ["blockedUsers", userId] as const,
};

// Query: inbound DM requests awaiting accept/decline.
export function useDMRequests() {
  const currentUser = useAppStore((state) => state.currentUser);

  return useQuery({
    queryKey: blocksQueryKeys.dmRequests(currentUser?.id ?? null),
    queryFn: async (): Promise<DmChannel[]> => {
      if (!currentUser) {
        return [];
      }
      return await invoke<DmChannel[]>("list_dm_requests", {
        userId: currentUser.id,
      });
    },
    enabled: !!currentUser,
    staleTime: 1000 * 30,
    refetchOnWindowFocus: true,
  });
}

// Mutation: accept a pending DM request, moving it into the conversations list.
export function useAcceptDMRequest() {
  const queryClient = useQueryClient();
  const currentUser = useAppStore((state) => state.currentUser);

  return useMutation({
    mutationFn: async (dmChannelId: string): Promise<void> => {
      if (!currentUser) {
        throw new Error("No current user");
      }
      await invoke("accept_dm_request", {
        dmChannelId,
        userId: currentUser.id,
      });
    },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["dmRequests"] });
      queryClient.invalidateQueries({ queryKey: ["dmConversations"] });
      // Also invalidate the existing dm-conversations key used by useDMConversations.
      queryClient.invalidateQueries({ queryKey: ["dm-conversations"] });
    },
  });
}

// Mutation: block a user. Any in-progress DM/channel with them should
// disappear from the conversations list on next refetch.
export function useBlockUser() {
  const queryClient = useQueryClient();
  const currentUser = useAppStore((state) => state.currentUser);

  return useMutation({
    mutationFn: async (blockedId: string): Promise<void> => {
      if (!currentUser) {
        throw new Error("No current user");
      }
      await invoke("block_user", {
        blockerId: currentUser.id,
        blockedId,
      });
    },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["dmRequests"] });
      queryClient.invalidateQueries({ queryKey: ["dmConversations"] });
      queryClient.invalidateQueries({ queryKey: ["dm-conversations"] });
      queryClient.invalidateQueries({ queryKey: ["blockedUsers"] });
    },
  });
}

// Mutation: unblock a user.
export function useUnblockUser() {
  const queryClient = useQueryClient();
  const currentUser = useAppStore((state) => state.currentUser);

  return useMutation({
    mutationFn: async (blockedId: string): Promise<void> => {
      if (!currentUser) {
        throw new Error("No current user");
      }
      await invoke("unblock_user", {
        blockerId: currentUser.id,
        blockedId,
      });
    },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["dmRequests"] });
      queryClient.invalidateQueries({ queryKey: ["dmConversations"] });
      queryClient.invalidateQueries({ queryKey: ["dm-conversations"] });
      queryClient.invalidateQueries({ queryKey: ["blockedUsers"] });
    },
  });
}

// Query: users the current user has blocked.
export function useBlockedUsers() {
  const currentUser = useAppStore((state) => state.currentUser);

  return useQuery({
    queryKey: blocksQueryKeys.blockedUsers(currentUser?.id ?? null),
    queryFn: async (): Promise<BlockedUser[]> => {
      if (!currentUser) {
        return [];
      }
      return await invoke<BlockedUser[]>("list_blocked_users", {
        userId: currentUser.id,
      });
    },
    enabled: !!currentUser,
    staleTime: 1000 * 60,
    refetchOnWindowFocus: true,
  });
}
