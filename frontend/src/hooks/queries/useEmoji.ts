import { useQuery, useMutation, useQueryClient } from "@tanstack/react-query";
import { useObserver } from "mobx-react-lite";
import { invoke } from "../../bridge";
import { appStore } from "../../stores/appStore";
import type { CustomEmoji } from "../../types";

export const emojiQueryKeys = {
  all: ["emoji"] as const,
  // Every emoji the user may send: the picker and the permission rule.
  usable: (userId: string | null) => ["emoji", "usable", userId] as const,
  group: (groupId: string) => ["emoji", "group", groupId] as const,
};

/** Every emoji `userId` may use (#848); the picker's only source, since it is the permission rule. */
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

/** One group's emoji (members-only, #917); rendering uses `get_emoji_url` by hash. */
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

/** Add an emoji from a path; Rust reads and re-encodes it. */
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
      // Invalidate all: every member's usable set changed.
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
