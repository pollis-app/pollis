import { useCallback, useMemo } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { invoke } from "../../bridge";

// Saved messages (#854).
//
// Every command behind these hooks is DEVICE-LOCAL — the `bookmark` table in
// the encrypted local SQLite DB. Nothing is sent to the Delivery Service or to
// Turso, because which messages a user saved is exactly the per-message
// interest metadata the threat model says an untrusted server must not learn.

/** Mirrors the Rust `SavedMessage` in `pollis-core/src/commands/bookmarks.rs`. */
export type SavedMessage = {
  message_id: string;
  conversation_id: string;
  /** When the user saved it — not when the message was sent. */
  saved_at: string;
  /** False once the local message body is gone (retention eviction, soft
   *  delete). The row still lists, as an honest placeholder. */
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

/**
 * The set of saved message ids, for rendering the per-message save affordance.
 *
 * Derived from the one saved-messages query rather than asking the backend per
 * message — a message list would otherwise fire one `invoke` per row.
 */
export function useSavedMessageIds(): Set<string> {
  const { data: saved = [] } = useSavedMessages();
  return useMemo(() => new Set(saved.map((s) => s.message_id)), [saved]);
}

/**
 * Toggle a message's saved state. Resolves to the state AFTER the toggle, so
 * callers can render the result without a re-read.
 */
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

/**
 * Resolve a permalink against THIS DEVICE'S local database.
 *
 * Imperative rather than a query because it runs in response to a paste/click,
 * and because caching a "not found" answer would be actively wrong once the
 * conversation's history finishes syncing.
 *
 * There is no server round-trip here and there must never be one: a
 * "fetch message by id" endpoint would turn a permalink from an inert pointer
 * into a way to pull content the device was never entitled to. A device that
 * was not in the MLS group when the message was sent never received the key
 * material, so the message is simply absent locally — the miss is cryptographic,
 * not a permission check.
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
        // A failed lookup is indistinguishable from a miss on purpose — the
        // caller must not be able to tell "error" from "you don't have this".
        return { found: false, conversation_id: null, message_id: null };
      }
    },
    [],
  );
}
