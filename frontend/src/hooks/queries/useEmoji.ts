import { useQuery, useMutation, useQueryClient } from "@tanstack/react-query";
import { useObserver } from "mobx-react-lite";
import { invoke } from "../../bridge";
import { appStore } from "../../stores/appStore";
import type { CustomEmoji } from "../../types";

export const emojiQueryKeys = {
  all: ["emoji"] as const,
  // Every custom emoji the user may SEND — the picker's source and the
  // permission rule in one query (see `list_usable_emoji` in Rust).
  usable: (userId: string | null) => ["emoji", "usable", userId] as const,
  // One group's emoji — the management page.
  group: (groupId: string) => ["emoji", "group", groupId] as const,
};

/**
 * Every custom emoji `userId` may use, across every group they are a member of.
 *
 * This is deliberately the ONLY source the picker draws its custom section from,
 * because it is also the permission predicate: a user may send any emoji from
 * any group they are in, in any conversation (#848, Discord's rule). Filtering
 * the picker from a different query than the one that decides permission is how
 * the two drift apart.
 *
 * Long `staleTime`: emoji change when someone adds one, which is rare, and the
 * add/remove mutations invalidate this key directly.
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
 * One group's custom emoji, for the group's emoji management page.
 *
 * #917: members-only. This does NOT affect rendering an emoji from a group you
 * are not in — that path is `useEmojiImage` → `get_emoji_url`, which takes a
 * content hash and no group, exactly as #848 requires.
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

/**
 * Add an emoji to a group from a file on disk.
 *
 * `path` is a filesystem path, not bytes: the Rust side reads, re-encodes and
 * uploads it, so a multi-megabyte source never crosses the JSON IPC.
 */
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
      // Invalidate the whole emoji tree: a new emoji changes both the group's
      // list and the usable set of every member, and the key for "every member"
      // is not knowable from here.
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
