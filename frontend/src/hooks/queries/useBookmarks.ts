import { useCallback, useMemo } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { invoke } from "../../bridge";

// Saved messages (#854): device-local only, since saves are interest metadata.

/** Mirrors the Rust `SavedMessage` in `pollis-core/src/commands/bookmarks.rs`. */
export type SavedMessage = {
  message_id: string;
  conversation_id: string;
  /** When the user saved it — not when the message was sent. */
  saved_at: string;
  /** False once the local body is gone; the row still lists. */
  available: boolean;
  content: string | null;
  sender_id: string | null;
  sent_at: string | null;
};

/** Mirrors the Rust `PermalinkTarget` in `pollis-core/src/commands/bookmarks.rs`. */
export type PermalinkTarget = {
  found: boolean;
  conversation_id: string | null;
  message_id: string | null;
};

export const bookmarkQueryKeys = {
  all: ["bookmarks"] as const,
  saved: ["bookmarks", "saved"] as const,
};

export function useSavedMessages() {
  return useQuery({
    queryKey: bookmarkQueryKeys.saved,
    queryFn: async (): Promise<SavedMessage[]> => {
      return (await invoke<SavedMessage[]>("list_saved_messages")) ?? [];
    },
    staleTime: 1000 * 30,
  });
}

/** Saved ids, derived from the one query. */
export function useSavedMessageIds(): Set<string> {
  const { data: saved = [] } = useSavedMessages();
  return useMemo(() => new Set(saved.map((s) => s.message_id)), [saved]);
}

/** Toggle saved; resolves to the new state. */
export function useToggleSavedMessage() {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: async (messageId: string): Promise<boolean> => {
      return await invoke<boolean>("toggle_saved_message", { messageId });
    },
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: bookmarkQueryKeys.all });
    },
  });
}

/** Unsave, used by the saved-items list (which has no "toggle" affordance). */
export function useUnsaveMessage() {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: async (messageId: string): Promise<boolean> => {
      return await invoke<boolean>("unsave_message", { messageId });
    },
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: bookmarkQueryKeys.all });
    },
  });
}

/** Resolve a permalink against the local DB only; never a server round-trip. Not cached. */
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
        // Errors look like misses on purpose.
        return { found: false, conversation_id: null, message_id: null };
      }
    },
    [],
  );
}
