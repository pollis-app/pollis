// Block / unblock hooks. The Rust side hides blocked-by-me DM channels
// in `list_dm_channels`, so toggling block state from the peer profile
// screen automatically prunes the inbox on the next refetch.

import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { invoke } from "../../lib/native";
import { useAppStore } from "../../stores/appStore";
import { dmQueryKeys } from "./useDMChannels";

export interface BlockedUser {
  blocked_id: string;
  blocked_username?: string;
  created_at: string;
}

export const blockQueryKeys = {
  list: (userId: string | null) => ["blocks", userId] as const,
};

export function useBlockedUsers() {
  const currentUser = useAppStore((s) => s.currentUser);
  return useQuery({
    queryKey: blockQueryKeys.list(currentUser?.id ?? null),
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
  });
}

export function useBlockUser() {
  const queryClient = useQueryClient();
  const currentUser = useAppStore((s) => s.currentUser);
  return useMutation({
    mutationFn: async (blockedId: string) => {
      if (!currentUser) {
        throw new Error("No current user");
      }
      await invoke("block_user", {
        blockerId: currentUser.id,
        blockedId,
      });
    },
    onSuccess: () => {
      queryClient.invalidateQueries({
        queryKey: blockQueryKeys.list(currentUser?.id ?? null),
      });
      queryClient.invalidateQueries({
        queryKey: dmQueryKeys.channels(currentUser?.id ?? null),
      });
    },
  });
}

export function useUnblockUser() {
  const queryClient = useQueryClient();
  const currentUser = useAppStore((s) => s.currentUser);
  return useMutation({
    mutationFn: async (blockedId: string) => {
      if (!currentUser) {
        throw new Error("No current user");
      }
      await invoke("unblock_user", {
        blockerId: currentUser.id,
        blockedId,
      });
    },
    onSuccess: () => {
      queryClient.invalidateQueries({
        queryKey: blockQueryKeys.list(currentUser?.id ?? null),
      });
      queryClient.invalidateQueries({
        queryKey: dmQueryKeys.channels(currentUser?.id ?? null),
      });
    },
  });
}
