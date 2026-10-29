import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { invoke } from "../../bridge";
import { appStore } from "../../stores/appStore";
import { useObserver } from "mobx-react-lite";
import { messageQueryKeys } from "./useMessages";
import type { BlockedUser, DmChannel } from "../../types";

export const blocksQueryKeys = {
  dmRequests: (userId: string | null) => ["dmRequests", userId] as const,
  blockedUsers: (userId: string | null) => ["blockedUsers", userId] as const,
};

export function useDMRequests() {
  const currentUser = useObserver(() => appStore.currentUser);

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
  });
}

export function useAcceptDMRequest() {
  const queryClient = useQueryClient();
  const currentUser = useObserver(() => appStore.currentUser);

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
      queryClient.invalidateQueries({ queryKey: messageQueryKeys.dmConversations(currentUser?.id ?? null) });
    },
  });
}

export function useBlockUser() {
  const queryClient = useQueryClient();
  const currentUser = useObserver(() => appStore.currentUser);

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
      queryClient.invalidateQueries({ queryKey: messageQueryKeys.dmConversations(currentUser?.id ?? null) });
      queryClient.invalidateQueries({ queryKey: ["blockedUsers"] });
    },
  });
}

export type ReportReason = "spam" | "harassment" | "illegal" | "other";

// Report a user (#1213), optionally a message and a block. Sends ids and a
// reason only; no message text leaves the device.
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
    }): Promise<void> => {
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
        queryClient.invalidateQueries({ queryKey: ["dmRequests"] });
        queryClient.invalidateQueries({ queryKey: messageQueryKeys.dmConversations(currentUser?.id ?? null) });
        queryClient.invalidateQueries({ queryKey: ["blockedUsers"] });
      }
    },
  });
}

export function useUnblockUser() {
  const queryClient = useQueryClient();
  const currentUser = useObserver(() => appStore.currentUser);

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
      queryClient.invalidateQueries({ queryKey: messageQueryKeys.dmConversations(currentUser?.id ?? null) });
      queryClient.invalidateQueries({ queryKey: ["blockedUsers"] });
    },
  });
}

export function useBlockedUsers() {
  const currentUser = useObserver(() => appStore.currentUser);

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
  });
}
