import { useMemo } from "react";
import { useMutation, useQuery, useQueryClient, type QueryClient } from "@tanstack/react-query";
import { useObserver } from "mobx-react-lite";
import { invoke } from "../../bridge";
import type { BotDetail, BotHost, BotSummary } from "../../types";
import { groupQueryKeys, useGroupMembers, useUserGroupsWithChannels } from "./useGroups";
import { appStore } from "../../stores/appStore";
import { botIdSet } from "../../utils/bots";

/** Bots (#1182). A bot is a group member too, so a change also refreshes the member list. */
const botQueryKeys = {
  all: ["bots"] as const,
  group: (groupId: string) => ["bots", "group", groupId] as const,
  bot: (botUserId: string) => ["bots", "bot", botUserId] as const,
};

function invalidateBots(queryClient: QueryClient, groupId: string | undefined) {
  queryClient.invalidateQueries({ queryKey: botQueryKeys.all });
  if (groupId) {
    queryClient.invalidateQueries({ queryKey: groupQueryKeys.members(groupId) });
  }
}

/** The group's bots. Off for non-admins: only admins manage them. */
export function useGroupBots(groupId: string, enabled: boolean) {
  return useQuery({
    queryKey: botQueryKeys.group(groupId),
    queryFn: async () => (await invoke<BotSummary[]>("list_group_bots", { groupId })) ?? [],
    enabled,
    staleTime: 1000 * 30,
  });
}

/** One bot with its hosts. */
export function useBot(botUserId: string, enabled: boolean) {
  return useQuery({
    queryKey: botQueryKeys.bot(botUserId),
    queryFn: () => invoke<BotDetail>("get_bot", { botUserId }),
    enabled,
    staleTime: 1000 * 30,
  });
}

/** Create a bot from the pairing code its first host printed. */
export function useCreateBot() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (vars: { groupId: string; code: string; displayName: string }) =>
      invoke<BotSummary>("create_bot", vars),
    onSuccess: (_bot, vars) => invalidateBots(queryClient, vars.groupId),
  });
}

/** Approve another host for a bot, by the code that host printed. */
export function useAddBotHost() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (vars: { botUserId: string; code: string; groupId: string }) =>
      invoke<BotHost>("add_bot_host", { botUserId: vars.botUserId, code: vars.code }),
    onSuccess: (_host, vars) => invalidateBots(queryClient, vars.groupId),
  });
}

/** Revoke one host (the bot's device); its other hosts keep running. */
export function useRevokeBotHost() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (vars: { botUserId: string; deviceId: string; groupId: string }) =>
      invoke<void>("revoke_bot_host", { botUserId: vars.botUserId, deviceId: vars.deviceId }),
    onSuccess: (_r, vars) => invalidateBots(queryClient, vars.groupId),
  });
}

/** Delete a bot: its account, its hosts and its membership. */
export function useDeleteBot() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (vars: { botUserId: string; groupId: string }) =>
      invoke<void>("delete_bot", { botUserId: vars.botUserId }),
    onSuccess: (_r, vars) => {
      queryClient.removeQueries({ queryKey: botQueryKeys.bot(vars.botUserId) });
      invalidateBots(queryClient, vars.groupId);
    },
  });
}

/** The group a Bots page is about, and whether the signed-in user may manage its bots. */
export function useBotGroup(groupId: string) {
  const { data: groupsWithChannels, isLoading } = useUserGroupsWithChannels();
  const group = groupsWithChannels?.find((g) => g.id === groupId);
  return { group, isAdmin: group?.current_user_role === "admin", isLoading };
}

/** The open channel's group's bots and its name, for Bot badges outside the message list. */
export function useSelectedGroupBots(): { botIds: ReadonlySet<string>; groupName?: string } {
  const groupId = useObserver(() => (appStore.selectedChannelId ? appStore.selectedGroupId : null));
  const { data: members } = useGroupMembers(groupId);
  const { data: groupsWithChannels } = useUserGroupsWithChannels();
  const botIds = useMemo(() => botIdSet(members ?? []), [members]);
  const groupName = groupId ? groupsWithChannels?.find((g) => g.id === groupId)?.name : undefined;
  return { botIds, groupName };
}
