import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { invoke } from "../../bridge";
import type { ApproveBotAppInput, BotAppView, GroupAppView } from "../../types";

/** Bot Apps (#2005): the admins' approval page and the members' marks. */
export const botAppQueryKeys = {
  all: ["botApps"] as const,
  app: (groupId: string, botUserId: string) => ["botApps", "app", groupId, botUserId] as const,
  group: (groupId: string) => ["botApps", "group", groupId] as const,
};

/** One bot's App as its admin page shows it: approved, waiting, and the runtime's report. */
export function useBotApp(groupId: string, botUserId: string, enabled: boolean) {
  return useQuery({
    queryKey: botAppQueryKeys.app(groupId, botUserId),
    queryFn: () => invoke<BotAppView>("get_bot_app", { groupId, botUserId }),
    enabled,
    staleTime: 1000 * 30,
  });
}

/** Every approved App in the group, for any member: the "Development build" and egress marks. */
export function useGroupApps(groupId: string | null | undefined) {
  return useQuery({
    queryKey: botAppQueryKeys.group(groupId ?? ""),
    queryFn: async () => (await invoke<GroupAppView[]>("list_group_apps", { groupId })) ?? [],
    enabled: !!groupId,
    staleTime: 1000 * 60,
  });
}

/** Sign and store an approval; the core returns the fresh view, which replaces the cached one. */
export function useApproveBotApp() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (input: ApproveBotAppInput) => invoke<BotAppView>("approve_bot_app", { input }),
    onSuccess: (view, input) => {
      queryClient.setQueryData(botAppQueryKeys.app(input.groupId, input.botUserId), view);
      queryClient.invalidateQueries({ queryKey: botAppQueryKeys.group(input.groupId) });
    },
  });
}

/** Refetch a bot's App after a refusal that means "look again" (stale or changed). */
export function useRefetchBotApp() {
  const queryClient = useQueryClient();
  return (groupId: string, botUserId: string) =>
    queryClient.invalidateQueries({ queryKey: botAppQueryKeys.app(groupId, botUserId) });
}

