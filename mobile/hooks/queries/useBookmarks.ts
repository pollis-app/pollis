// Saved messages (#887) + permalink resolution over device-local core commands.

import { useCallback } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { invoke } from "../../lib/native";

export interface SavedMessage {
  message_id: string;
  conversation_id: string;
  /** When the user saved it. */
  saved_at: string;
  available: boolean;
  content: string | null;
  sender_id: string | null;
  sent_at: string | null;
}

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

export function useSavedMessageIds(): Set<string> {
  const { data } = useSavedMessages();
  return new Set((data ?? []).map((m) => m.message_id));
}

export function useToggleSavedMessage() {
  const queryClient = useQueryClient();
  return useMutation({
    // Tactile feedback (#1286): lib/queryClient.ts plays it.
    meta: { haptic: { success: "toggle" } },
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
    // Tactile feedback (#1286): lib/queryClient.ts plays it.
    meta: { haptic: { success: "toggle" } },
    mutationFn: async (messageId: string) =>
      await invoke<boolean>("unsave_message", { messageId }),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: bookmarkQueryKeys.all });
    },
  });
}

/** A thrown lookup is returned as a miss, on purpose (non-oracle). */
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
