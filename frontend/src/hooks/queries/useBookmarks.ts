import { useCallback, useMemo } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { invoke } from "../../bridge";

// Saved messages (#854): DEVICE-LOCAL only (the encrypted local `bookmark`
// table). What a user saved is interest metadata no server may learn.

/** Mirrors the Rust `SavedMessage` in `pollis-core/src/commands/bookmarks.rs`. */
export type SavedMessage = {
  message_id: string;
  conversation_id: string;
  /** When the user saved it, not when it was sent. */
  saved_at: string;
  /** False once the local body is gone (retention, soft delete); still listed. */
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

/** Saved message ids, derived from the one saved-messages query (not per row). */
export function useSavedMessageIds(): Set<string> {
  const { data: saved = [] } = useSavedMessages();
  return useMemo(() => new Set(saved.map((s) => s.message_id)), [saved]);
}

/** Toggle saved state; resolves to the state AFTER the toggle. */
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

/**
 * Resolve a permalink against THIS DEVICE'S local database. Imperative, since a
 * cached "not found" goes stale as history syncs. Never a server round-trip:
 * a fetch-by-id endpoint would hand out content the device was never keyed for.
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
        // Errors are indistinguishable from a miss on purpose.
        return { found: false, conversation_id: null, message_id: null };
      }
    },
    [],
  );
}
