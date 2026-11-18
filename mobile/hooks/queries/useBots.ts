// Bots (#1182): a group's bots and their hosts. Desktop calls the same six bridge commands
// (lane contract); the core refuses a non-admin, so these hooks only mirror what it allows.

import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useObserver } from "mobx-react-lite";
import { invoke } from "../../lib/native";
import { appStore } from "../../stores/appStore";
import type { BotDetail, BotHost, BotSummary } from "../../lib/bots";
import { groupInviteQueryKeys } from "./useGroupInvites";
import { securityEventQueryKeys } from "./useSecurityEvents";

/** Under `groups`, so a group-wide invalidation (deleting the group) covers them. */
export const botQueryKeys = {
  all: ["groups", "bots"] as const,
  group: (groupId: string | null) => ["groups", "bots", "group", groupId] as const,
  bot: (botUserId: string | null) => ["groups", "bots", "bot", botUserId] as const,
};

/** The group's bots. Empty outside a group. */
export function useGroupBots(groupId: string | null) {
  const currentUser = useObserver(() => appStore.currentUser);
  return useQuery({
    queryKey: botQueryKeys.group(groupId),
    queryFn: async (): Promise<BotSummary[]> => {
      if (!groupId) {
        return [];
      }
      return (await invoke<BotSummary[]>("list_group_bots", { groupId })) ?? [];
    },
    enabled: !!groupId && !!currentUser,
    staleTime: 1000 * 30,
  });
}

/** One bot with its hosts and its group's bot cap. */
export function useBot(botUserId: string | null) {
  const currentUser = useObserver(() => appStore.currentUser);
  return useQuery({
    queryKey: botQueryKeys.bot(botUserId),
    queryFn: async (): Promise<BotDetail | null> => {
      if (!botUserId) {
        return null;
      }
      return await invoke<BotDetail>("get_bot", { botUserId });
    },
    enabled: !!botUserId && !!currentUser,
    staleTime: 1000 * 30,
  });
}

/** Everything a bot change can move: the list, the bot, the roster it sits in, the audit list. */
function invalidateBots(queryClient: ReturnType<typeof useQueryClient>, groupId: string | null) {
  queryClient.invalidateQueries({ queryKey: botQueryKeys.all });
  queryClient.invalidateQueries({ queryKey: groupInviteQueryKeys.members(groupId) });
  queryClient.invalidateQueries({ queryKey: securityEventQueryKeys.all });
}

/** Create a bot from the pairing code its first host printed. The admin TYPED `code` (#1096). */
export function useCreateBot(groupId: string | null) {
  const queryClient = useQueryClient();
  return useMutation({
    // Tactile feedback (#1286): lib/queryClient.ts plays it.
    meta: { haptic: { success: "success" } },
    mutationFn: async (vars: { code: string; displayName: string }): Promise<BotSummary> => {
      if (!groupId) {
        throw new Error("No group");
      }
      return await invoke<BotSummary>("create_bot", {
        groupId,
        code: vars.code,
        displayName: vars.displayName,
      });
    },
    onSuccess: () => invalidateBots(queryClient, groupId),
  });
}

/** Approve a new host for an existing bot, by the code that host printed. */
export function useAddBotHost(botUserId: string | null, groupId: string | null) {
  const queryClient = useQueryClient();
  return useMutation({
    // Tactile feedback (#1286): lib/queryClient.ts plays it.
    meta: { haptic: { success: "success" } },
    mutationFn: async (code: string): Promise<BotHost> => {
      if (!botUserId) {
        throw new Error("No bot");
      }
      return await invoke<BotHost>("add_bot_host", { botUserId, code });
    },
    onSuccess: () => invalidateBots(queryClient, groupId),
  });
}

/** Revoke one host (an ordinary device revoke on the bot's user). */
export function useRevokeBotHost(botUserId: string | null, groupId: string | null) {
  const queryClient = useQueryClient();
  return useMutation({
    // Tactile feedback (#1286): lib/queryClient.ts plays it.
    meta: { haptic: { success: "destructive" } },
    mutationFn: async (deviceId: string) => {
      if (!botUserId) {
        throw new Error("No bot");
      }
      await invoke("revoke_bot_host", { botUserId, deviceId });
    },
    onSuccess: () => invalidateBots(queryClient, groupId),
  });
}

/** Delete the bot: its account, its hosts and its membership. */
export function useDeleteBot(groupId: string | null) {
  const queryClient = useQueryClient();
  return useMutation({
    // Tactile feedback (#1286): lib/queryClient.ts plays it.
    meta: { haptic: { success: "destructive" } },
    mutationFn: async (botUserId: string) => {
      await invoke("delete_bot", { botUserId });
    },
    onSuccess: (_data, botUserId) => {
      queryClient.removeQueries({ queryKey: botQueryKeys.bot(botUserId) });
      invalidateBots(queryClient, groupId);
    },
  });
}
