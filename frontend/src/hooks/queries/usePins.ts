import { useMemo } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useObserver } from "mobx-react-lite";
import { invoke } from "../../bridge";
import { appStore } from "../../stores/appStore";

/**
 * Pinned messages (#99).
 *
 * Pins are per-CONVERSATION shared state — every member sees the same list —
 * so unlike bookmarks (deliberately device-local, see `useBookmarks`) these
 * hooks are backed by the DS: the Rust commands seal/open pin content under
 * the conversation's pin master key and the server stores only ciphertext.
 */

/** Mirrors the Rust PinnedMessage in pollis-core/src/commands/pinned_messages.rs. */
export interface PinnedMessage {
  id: string;
  conversation_id: string;
  message_id: string;
  pinned_by: string;
  pinned_at: string;
  content: string;
  sender_id: string;
  sender_username?: string | null;
  sent_at: string;
}

export const pinQueryKeys = {
  all: ["pins"] as const,
  conversation: (conversationId: string | null) =>
    ["pins", conversationId] as const,
};

/**
 * One conversation's pins, newest first, decrypted in Rust.
 *
 * `conversationId` is the channel or DM id. The list can legitimately error
 * while the device is still waiting for the conversation's pin key (a fresh
 * external join); React Query surfaces that as `error` and retries on the
 * next mount, which matches the "heals on the next pass" design.
 */
export function usePinnedMessages(conversationId: string | null) {
  const currentUser = useObserver(() => appStore.currentUser);
  return useQuery({
    queryKey: pinQueryKeys.conversation(conversationId),
    queryFn: async (): Promise<PinnedMessage[]> =>
      (await invoke<PinnedMessage[]>("list_pinned_messages", {
        conversationId,
        userId: currentUser!.id,
      })) ?? [],
    enabled: Boolean(conversationId && currentUser),
    staleTime: 1000 * 30,
  });
}

/** The pinned message ids for a conversation, for per-row pin indicators. */
export function usePinnedMessageIds(conversationId: string | null): Set<string> {
  const { data: pins = [] } = usePinnedMessages(conversationId);
  return useMemo(() => new Set(pins.map((p) => p.message_id)), [pins]);
}

interface TogglePinVars {
  conversationId: string;
  messageId: string;
  /** Current state — `true` means this toggle UNPINS. */
  isPinned: boolean;
}

/** Pin or unpin one message; any member may do either. */
export function useTogglePinnedMessage() {
  const queryClient = useQueryClient();
  const currentUser = useObserver(() => appStore.currentUser);
  return useMutation({
    mutationFn: async ({ conversationId, messageId, isPinned }: TogglePinVars) => {
      if (!currentUser) {
        throw new Error("Not signed in");
      }
      await invoke(isPinned ? "unpin_message" : "pin_message", {
        conversationId,
        messageId,
        userId: currentUser.id,
      });
    },
    onSuccess: (_data, { conversationId }) => {
      void queryClient.invalidateQueries({
        queryKey: pinQueryKeys.conversation(conversationId),
      });
    },
  });
}
