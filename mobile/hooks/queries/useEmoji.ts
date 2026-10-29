import {
  useMutation,
  useQuery,
  useQueryClient,
} from "@tanstack/react-query";
import { invoke } from "../../lib/native";
import { appStore } from "../../stores/appStore";
import { useObserver } from "mobx-react-lite";

export interface CustomEmoji {
  group_id: string;
  group_name: string;
  shortcode: string;
  content_hash: string;
  content_type: string;
  animated: boolean;
  size_bytes: number;
  created_by: string;
}

/** Shortcode grammar — must agree with `shortcode_is_valid` in emoji.rs. */
export const SHORTCODE_RE = /^[a-z0-9_]{2,32}$/;

export const emojiQueryKeys = {
  all: ["emoji"] as const,
  usable: (userId: string | null) => ["emoji", "usable", userId] as const,
  group: (groupId: string | null) => ["emoji", "group", groupId] as const,
};

/** Every custom emoji from all the user's groups, usable in any conversation. */
export function useUsableEmoji() {
  const currentUser = useObserver(() => appStore.currentUser);
  const userId = currentUser?.id ?? null;
  return useQuery({
    queryKey: emojiQueryKeys.usable(userId),
    queryFn: async (): Promise<CustomEmoji[]> => {
      if (!userId) {
        return [];
      }
      return (await invoke<CustomEmoji[]>("list_usable_emoji", { userId })) ?? [];
    },
    enabled: !!userId,
    staleTime: 1000 * 60,
  });
}

export function useGroupEmoji(groupId: string | null) {
  const currentUser = useObserver(() => appStore.currentUser);
  return useQuery({
    queryKey: emojiQueryKeys.group(groupId),
    queryFn: async (): Promise<CustomEmoji[]> => {
      if (!groupId || !currentUser) {
        return [];
      }
      return (
        (await invoke<CustomEmoji[]>("list_group_emoji", {
          groupId,
          requesterId: currentUser.id,
        })) ?? []
      );
    },
    enabled: !!(groupId && currentUser),
    staleTime: 1000 * 30,
  });
}

/** Upload a group emoji from a local image; Rust re-encodes it to a <=48 KiB webp/gif. */
export function useUploadGroupEmoji(groupId: string | null) {
  const queryClient = useQueryClient();
  return useMutation({
    meta: { haptic: { success: "success" } },
    mutationFn: async (vars: { shortcode: string; path: string }) => {
      if (!groupId) {
        throw new Error("No group");
      }
      return await invoke<CustomEmoji>("upload_group_emoji", {
        groupId,
        shortcode: vars.shortcode,
        path: vars.path,
      });
    },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: emojiQueryKeys.all });
    },
  });
}

export function useRemoveGroupEmoji(groupId: string | null) {
  const queryClient = useQueryClient();
  return useMutation({
    meta: { haptic: { success: "destructive" } },
    mutationFn: async (shortcode: string) => {
      if (!groupId) {
        throw new Error("No group");
      }
      await invoke("remove_group_emoji", { groupId, shortcode });
    },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: emojiQueryKeys.all });
    },
  });
}
