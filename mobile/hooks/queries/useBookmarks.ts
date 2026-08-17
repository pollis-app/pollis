// Saved messages (bookmarks, #887) + permalink resolution. Mirrors
// desktop's useBookmarks.ts over the device-local core commands in
// pollis-core/src/commands/bookmarks.rs — all snake_case fields as serde
// serializes them.

import { useCallback } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { invoke } from "../../lib/native";

/** Mirror of `pollis_core::commands::bookmarks::SavedMessage`. */
export interface SavedMessage {
  message_id: string;
  conversation_id: string;
  /** When the user saved it (not when the message was sent). */
  saved_at: string;
  /** True iff this device still holds the message body. */
  available: boolean;
  content: string | null;
  sender_id: string | null;
  sent_at: string | null;
}

/** Mirror of `pollis_core::commands::bookmarks::PermalinkTarget`. */
export interface PermalinkTarget {
  found: boolean;
  conversation_id: string | null;
  message_id: string | null;
}

export const bookmarkQueryKeys = {
  all: ["bookmarks"] as const,
  saved: ["bookmarks", "saved"] as const,
};

export function useSavedMessages() {
  return useQuery({
    queryKey: bookmarkQueryKeys.saved,
    queryFn: async (): Promise<SavedMessage[]> =>
      (await invoke<SavedMessage[]>("list_saved_messages")) ?? [],
    staleTime: 1000 * 30,
  });
}

/** The saved ids as a set — one query feeds every row's saved state. */
export function useSavedMessageIds(): Set<string> {
  const { data } = useSavedMessages();
  return new Set((data ?? []).map((m) => m.message_id));
}

/** Returns the new saved state (true = now saved). */
export function useToggleSavedMessage() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: async (messageId: string) =>
      await invoke<boolean>("toggle_saved_message", { messageId }),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: bookmarkQueryKeys.all });
    },
  });
}

export function useUnsaveMessage() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: async (messageId: string) =>
      await invoke<boolean>("unsave_message", { messageId }),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: bookmarkQueryKeys.all });
    },
  });
}

/**
 * Imperative permalink resolution. A thrown lookup is returned as a miss —
 * a failed lookup is indistinguishable from a message this device does not
 * hold, on purpose (non-oracle).
 */
export function useResolvePermalink() {
  return useCallback(
    async (
      conversationId: string,
      messageId: string,
    ): Promise<PermalinkTarget> => {
      try {
        return await invoke<PermalinkTarget>("resolve_message_permalink", {
          conversationId,
          messageId,
        });
      } catch {
        return { found: false, conversation_id: null, message_id: null };
      }
    },
    [],
  );
}
