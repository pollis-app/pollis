import { useMemo } from "react";
import { useMutation, useQuery, useQueryClient, type QueryClient } from "@tanstack/react-query";
import { useObserver } from "mobx-react-lite";
import { invoke } from "../../bridge";
import { appStore } from "../../stores/appStore";
import { aliasName, toAliasMap, type AliasMap, type GroupAlias } from "../../utils/groupAlias";

/** Per-group aliases (#2003). Under `groups`, so a group-wide invalidation covers them. */
export const aliasQueryKeys = {
  all: ["groups", "aliases"] as const,
  group: (groupId: string) => ["groups", "aliases", groupId] as const,
  mine: (userId: string | null) => ["groups", "aliases", "mine", userId] as const,
};

/** Every alias in `groupId` (members past and present), by user id. Empty outside a group. */
export function useGroupAliases(groupId: string | null | undefined): AliasMap {
  const currentUser = useObserver(() => appStore.currentUser);
  const { data } = useQuery({
    queryKey: aliasQueryKeys.group(groupId ?? ""),
    queryFn: async (): Promise<GroupAlias[]> => {
      if (!groupId || !currentUser) {
        return [];
      }
      return (
        (await invoke<GroupAlias[]>("list_group_aliases", {
          groupId,
          requesterId: currentUser.id,
        })) ?? []
      );
    },
    enabled: !!groupId && !!currentUser,
    staleTime: 1000 * 30,
  });
  return useMemo(() => toAliasMap(data), [data]);
}

/**
 * The name a channel message's sender shows in that channel's group, for an OS
 * notification (#2003): the cached aliases when fresh, else one read. Falls back to
 * `fallback` outside a group, for a channel this device has not listed, or on error.
 */
export async function channelAliasName(
  queryClient: QueryClient,
  requesterId: string,
  channelId: string,
  senderId: string | null | undefined,
  fallback: string,
): Promise<string> {
  if (!senderId) {
    return fallback;
  }
  const groups = queryClient.getQueryData<Array<{ id: string; channels?: Array<{ id: string }> }>>(
    ["groups", "with-channels", requesterId],
  );
  const groupId = groups?.find((g) => g.channels?.some((c) => c.id === channelId))?.id;
  if (!groupId) {
    return fallback;
  }
  try {
    const list = await queryClient.fetchQuery({
      queryKey: aliasQueryKeys.group(groupId),
      queryFn: async () =>
        (await invoke<GroupAlias[]>("list_group_aliases", { groupId, requesterId })) ?? [],
      staleTime: 1000 * 30,
    });
    return aliasName(toAliasMap(list), senderId, fallback);
  } catch {
    return fallback;
  }
}

/** The open channel's group aliases; empty in a DM or with no channel open. */
export function useSelectedGroupAliases(): AliasMap {
  const groupId = useObserver(() =>
    appStore.selectedChannelId ? appStore.selectedGroupId : null,
  );
  return useGroupAliases(groupId);
}

/** Every alias this account owns, by group id. */
export function useMyAliases() {
  const currentUser = useObserver(() => appStore.currentUser);
  return useQuery({
    queryKey: aliasQueryKeys.mine(currentUser?.id ?? null),
    queryFn: async (): Promise<Map<string, GroupAlias>> => {
      if (!currentUser) {
        return new Map();
      }
      const list = (await invoke<GroupAlias[]>("list_my_aliases", { userId: currentUser.id })) ?? [];
      return new Map(list.map((a) => [a.group_id, a]));
    },
    enabled: !!currentUser,
    staleTime: 1000 * 30,
  });
}

/**
 * Write the signed-in user's alias. `groupId: null` writes to every group they are
 * currently in ("apply to all"). Per field: omitted leaves it, `""` clears it.
 */
export function useSetGroupAlias() {
  const queryClient = useQueryClient();
  const currentUser = useObserver(() => appStore.currentUser);
  return useMutation({
    mutationFn: async (vars: {
      groupId: string | null;
      displayName?: string;
      avatarUrl?: string;
    }) => {
      if (!currentUser) {
        throw new Error("No current user");
      }
      await invoke("set_group_alias", {
        userId: currentUser.id,
        groupId: vars.groupId,
        displayName: vars.displayName,
        avatarUrl: vars.avatarUrl,
      });
      return vars.groupId;
    },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: aliasQueryKeys.all });
    },
  });
}

/** Delete `userId`'s alias in `groupId`: the owner's "Use my main profile", or an admin's reset. */
export function useResetGroupAlias() {
  const queryClient = useQueryClient();
  const currentUser = useObserver(() => appStore.currentUser);
  return useMutation({
    mutationFn: async ({ groupId, userId }: { groupId: string; userId: string }) => {
      if (!currentUser) {
        throw new Error("No current user");
      }
      await invoke("reset_group_alias", { groupId, userId, requesterId: currentUser.id });
      return groupId;
    },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: aliasQueryKeys.all });
    },
  });
}
