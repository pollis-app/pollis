// Per-group aliases (#2003): a member's name and picture in one group. Desktop's
// `frontend/src/hooks/queries/useGroupAliases.ts`, same commands.

import { useMemo } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useObserver } from "mobx-react-lite";
import { invoke } from "../../lib/native";
import { appStore } from "../../stores/appStore";
import { toAliasMap, type AliasMap, type GroupAlias } from "../../lib/groupAlias";
import { AVATAR_RENDITIONS } from "../../lib/avatarKey";
import { AVATAR_DIR, avatarStore } from "../../lib/avatarCache";

/** Under `groups`, so a group-wide invalidation covers them. */
export const aliasQueryKeys = {
  all: ["groups", "aliases"] as const,
  group: (groupId: string | null) => ["groups", "aliases", groupId] as const,
  mine: (userId: string | null) => ["groups", "aliases", "mine", userId] as const,
};

/** Every alias in `groupId` (members past and present), by user id. Empty outside a group. */
export function useGroupAliases(groupId: string | null | undefined): AliasMap {
  const currentUser = useObserver(() => appStore.currentUser);
  const { data } = useQuery({
    queryKey: aliasQueryKeys.group(groupId ?? null),
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
    // Tactile feedback (#1286): lib/queryClient.ts plays it.
    meta: { haptic: { success: "success" } },
    mutationFn: async (vars: { groupId: string | null; displayName?: string; avatarUrl?: string }) => {
      if (!currentUser) {
        throw new Error("No current user");
      }
      await invoke("set_group_alias", {
        userId: currentUser.id,
        groupId: vars.groupId,
        displayName: vars.displayName,
        avatarUrl: vars.avatarUrl,
      });
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
    // Tactile feedback (#1286): lib/queryClient.ts plays it.
    meta: { haptic: { success: "destructive" } },
    mutationFn: async (vars: { groupId: string; userId: string }) => {
      if (!currentUser) {
        throw new Error("No current user");
      }
      await invoke("reset_group_alias", {
        groupId: vars.groupId,
        userId: vars.userId,
        requesterId: currentUser.id,
      });
    },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: aliasQueryKeys.all });
    },
  });
}

/**
 * Upload a picture for an alias without touching the main profile: core re-encodes it
 * (square, no EXIF), uploads it content-addressed and writes its renditions, which the
 * avatar store adopts as an alias picture. Returns the key `set_group_alias` names.
 */
export async function uploadAliasPicture(path: string): Promise<string> {
  const userId = appStore.currentUser?.id;
  if (!userId) {
    throw new Error("No current user");
  }
  const result = await invoke<{ key: string; uri?: string | null }>("upload_avatar_image", {
    userId,
    path,
    px: AVATAR_RENDITIONS[AVATAR_RENDITIONS.length - 1],
    destDir: AVATAR_DIR,
  });
  avatarStore.adopt(userId, result.key, result.uri ?? null, { alias: true });
  return result.key;
}
