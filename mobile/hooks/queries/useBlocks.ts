// Block / unblock hooks. The Rust side hides blocked-by-me DM channels
// in `list_dm_channels`, so toggling block state from the peer profile
// screen automatically prunes the inbox on the next refetch.

import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { invoke } from "../../lib/native";
import { appStore } from "../../stores/appStore";
import { useObserver } from "mobx-react-lite";
import { dmQueryKeys } from "./useDMChannels";

// Mirrors `pollis_core::commands::blocks::BlockedUser` field for field. It
// used to name these `blocked_id` / `blocked_username` / `created_at`, which
// the core never sends, so a blocked profile never offered Unblock and the
// Blocked Users list read `undefined`.
export interface BlockedUser {
  user_id: string;
  username?: string | null;
  blocked_at: string;
}

export const blockQueryKeys = {
  list: (userId: string | null) => ["blocks", userId] as const,
};

export function useBlockedUsers() {
  const currentUser = useObserver(() => appStore.currentUser);
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
  const currentUser = useObserver(() => appStore.currentUser);
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
      // Blocking is also the decline path for a DM request (desktop's
      // RequestsPage does the same) — drop the sender's pending request row.
      queryClient.invalidateQueries({
        queryKey: dmQueryKeys.requests(currentUser?.id ?? null),
      });
    },
  });
}

export type ReportReason = "spam" | "harassment" | "illegal" | "other";

/**
 * Report a user (#1213), optionally pointing at one of their messages,
 * optionally blocking them in the same step. Sends ids and a reason only; no
 * message text ever leaves the device.
 */
export function useReportUser() {
  const queryClient = useQueryClient();
  const currentUser = useObserver(() => appStore.currentUser);
  return useMutation({
    mutationFn: async (vars: {
      reportedId: string;
      reason: ReportReason;
      conversationId?: string | null;
      messageId?: string | null;
      alsoBlock: boolean;
    }) => {
      await invoke("report_user", {
        reportedId: vars.reportedId,
        reason: vars.reason,
        conversationId: vars.conversationId ?? null,
        messageId: vars.messageId ?? null,
        alsoBlock: vars.alsoBlock,
      });
    },
    onSuccess: (_data, vars) => {
      if (vars.alsoBlock) {
        queryClient.invalidateQueries({ queryKey: blockQueryKeys.list(currentUser?.id ?? null) });
        queryClient.invalidateQueries({ queryKey: dmQueryKeys.channels(currentUser?.id ?? null) });
        queryClient.invalidateQueries({ queryKey: dmQueryKeys.requests(currentUser?.id ?? null) });
      }
    },
  });
}

export function useUnblockUser() {
  const queryClient = useQueryClient();
  const currentUser = useObserver(() => appStore.currentUser);
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
