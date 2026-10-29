import { useQuery, useMutation, useQueryClient } from "@tanstack/react-query";
import { useObserver } from "mobx-react-lite";
import { invoke } from "../../bridge";
import { appStore } from "../../stores/appStore";
import type { CustomEmoji } from "../../types";

export const emojiQueryKeys = {
  all: ["emoji"] as const,
  // Every custom emoji the user may SEND (`list_usable_emoji`).
  usable: (userId: string | null) => ["emoji", "usable", userId] as const,
  group: (groupId: string) => ["emoji", "group", groupId] as const,
};

/**
 * Every custom emoji `userId` may use, across all their groups, in any
 * conversation (#848). The picker's ONLY custom source, because it is also
 * the permission predicate. Mutations invalidate it.
 */
export function useUsableEmoji(userId: string | null) {
  return useQuery({
    queryKey: emojiQueryKeys.usable(userId),
    queryFn: async (): Promise<CustomEmoji[]> => {
      return await invoke<CustomEmoji[]>("list_usable_emoji", { userId });
    },
    enabled: !!userId,
    staleTime: 1000 * 60 * 5,
  });
}

/**
 * One group's emoji for its management page; members-only (#917). Rendering
 * goes through `get_emoji_url` by hash, with no group.
 */
export function useGroupEmoji(groupId: string) {
  const currentUser = useObserver(() => appStore.currentUser);

  return useQuery({
    queryKey: emojiQueryKeys.group(groupId),
    queryFn: async (): Promise<CustomEmoji[]> => {
      return await invoke<CustomEmoji[]>("list_group_emoji", {
        groupId,
        requesterId: currentUser?.id,
      });
    },
    enabled: !!groupId && !!currentUser,
    staleTime: 1000 * 60,
  });
}

/** Add an emoji from a file path; Rust reads, re-encodes and uploads it. */
export function useUploadGroupEmoji() {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: async ({
      groupId,
      shortcode,
      path,
    }: {
      groupId: string;
      shortcode: string;
      path: string;
    }) => {
      return await invoke<CustomEmoji>("upload_group_emoji", {
        groupId,
        shortcode,
        path,
      });
    },
    onSuccess: () => {
      // The whole tree: every member's usable set changes too.
      queryClient.invalidateQueries({ queryKey: emojiQueryKeys.all });
    },
  });
}

export function useRemoveGroupEmoji() {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: async ({
      groupId,
      shortcode,
    }: {
      groupId: string;
      shortcode: string;
    }) => {
      await invoke("remove_group_emoji", { groupId, shortcode });
    },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: emojiQueryKeys.all });
    },
  });
}
