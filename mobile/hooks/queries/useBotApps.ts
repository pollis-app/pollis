// Bot Apps (#2005): a bot's App as its admins see it, the approval, and the members' read of the
// group's approved Apps. Desktop calls the same three bridge commands; the core refuses what the
// caller may not do. Automatic updates happen in the bot's runtime, never in an admin's app.

import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useObserver } from "mobx-react-lite";
import { invoke } from "../../lib/native";
import { appStore } from "../../stores/appStore";
import { botQueryKeys } from "./useBots";
import {
  isStaleApprovalError,
  type ApproveBotAppInput,
  type BotAppView,
  type GroupAppView,
} from "../../lib/botApps";

/** Under `groups`, so a group-wide invalidation (deleting the group) covers them. */
export const botAppQueryKeys = {
  all: ["groups", "botApps"] as const,
  app: (groupId: string | null, botUserId: string | null) => ["groups", "botApps", "app", groupId, botUserId] as const,
  group: (groupId: string | null) => ["groups", "botApps", "group", groupId] as const,
};

/** One bot's App for an admin: approved, waiting, and what its runtime reports. */
export function useBotApp(groupId: string | null, botUserId: string | null, enabled = true) {
  const currentUser = useObserver(() => appStore.currentUser);
  return useQuery({
    queryKey: botAppQueryKeys.app(groupId, botUserId),
    queryFn: async (): Promise<BotAppView | null> => {
      if (!groupId || !botUserId) {
        return null;
      }
      return await invoke<BotAppView>("get_bot_app", { groupId, botUserId });
    },
    enabled: enabled && !!groupId && !!botUserId && !!currentUser,
    staleTime: 1000 * 15,
  });
}

/** Every approved App in the group that verifies on this device; any member may read it. */
export function useGroupApps(groupId: string | null) {
  const currentUser = useObserver(() => appStore.currentUser);
  return useQuery({
    queryKey: botAppQueryKeys.group(groupId),
    queryFn: async (): Promise<GroupAppView[]> => {
      if (!groupId) {
        return [];
      }
      return (await invoke<GroupAppView[]>("list_group_apps", { groupId })) ?? [];
    },
    enabled: !!groupId && !!currentUser,
    staleTime: 1000 * 60,
  });
}

/**
 * Sign and store an approval (first approval, update, or narrowing in place). The fresh view comes
 * back and replaces the cached one; an out-of-date page (another admin was first, or the build
 * changed) is refetched so the admin looks again.
 */
export function useApproveBotApp(groupId: string | null, botUserId: string | null) {
  const queryClient = useQueryClient();
  return useMutation({
    // Tactile feedback (#1286): lib/queryClient.ts plays it.
    meta: { haptic: { success: "success" } },
    mutationFn: async (input: ApproveBotAppInput): Promise<BotAppView> => {
      return await invoke<BotAppView>("approve_bot_app", { input });
    },
    onSuccess: (view) => {
      queryClient.setQueryData(botAppQueryKeys.app(groupId, botUserId), view);
      queryClient.invalidateQueries({ queryKey: botAppQueryKeys.group(groupId) });
      // The approved App's slash commands (#2018) change with it; the composer re-reads them.
      queryClient.invalidateQueries({ queryKey: botQueryKeys.commands(groupId) });
    },
    onError: (error) => {
      if (isStaleApprovalError((error as Error)?.message)) {
        queryClient.invalidateQueries({ queryKey: botAppQueryKeys.app(groupId, botUserId) });
      }
    },
  });
}

