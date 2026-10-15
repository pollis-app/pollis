// Pinned messages (#99; mobile #1315). Port of desktop's
// frontend/src/hooks/queries/usePins.ts over the same bridge commands
// (`pin_message` / `unpin_message` / `list_pinned_messages`, pollis-core's
// commands/pinned_messages.rs).
//
// Pins are per-CONVERSATION shared state — every member sees the same list,
// and any member may pin or unpin (the DS gates on membership only) — so
// unlike saved messages they are DS-backed: Rust seals and opens each pin's
// content snapshot under the conversation's pin key, the server stores only
// ciphertext. `conversationId` is the channel or DM id, never the MLS group.

import { useMemo } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useObserver } from "mobx-react-lite";
import { invoke } from "../../lib/native";
import { appStore } from "../../stores/appStore";
import { pinnedIds, type PinnedMessage } from "../../lib/pins";

export type { PinnedMessage } from "../../lib/pins";

export const pinQueryKeys = {
  all: ["pins"] as const,
  conversation: (conversationId: string | null) =>
    ["pins", conversationId] as const,
};

/**
 * One conversation's pins, newest first, decrypted in Rust. The query can
 * legitimately error while this device waits for the conversation's pin key
 * (a fresh join); the screen words that as "not available yet", and the
 * next mount retries — desktop's "heals on the next pass".
 *
 * `fresh`: refetch on every mount, whatever the cache holds. The pinned list
 * passes it — pins change with no realtime event (another member pins or
 * unpins), and the chat screen has usually just cached the list for its
 * message sheet, so a 30 s-fresh copy would hide a pin made a moment ago.
 */
export function usePinnedMessages(
  conversationId: string | null,
  { fresh = false }: { fresh?: boolean } = {},
) {
  const currentUser = useObserver(() => appStore.currentUser);
  return useQuery({
    refetchOnMount: fresh ? "always" : true,
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

/** The pinned message ids, for the message sheet's Pin / Unpin row. */
export function usePinnedMessageIds(conversationId: string | null): Set<string> {
  const { data } = usePinnedMessages(conversationId);
  return useMemo(() => pinnedIds(data ?? []), [data]);
}

export type TogglePinVars = {
  conversationId: string;
  messageId: string;
  /** Current state — `true` means this toggle UNPINS. */
  isPinned: boolean;
};

/** Pin or unpin one message; any member may do either. */
export function useTogglePinnedMessage() {
  const queryClient = useQueryClient();
  const currentUser = useObserver(() => appStore.currentUser);
  return useMutation({
    // Tactile feedback (#1286): lib/queryClient.ts plays it.
    meta: { haptic: { success: "toggle" } },
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
