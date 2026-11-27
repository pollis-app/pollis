import { useMemo } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useObserver } from "mobx-react-lite";
import { invoke } from "../../bridge";
import { appStore } from "../../stores/appStore";
import type { LinkPreviewWire } from "../../types";

/** Pinned messages (#99): shared per conversation, sealed under its pin key; the DS holds ciphertext only. */

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
  sender_display_name?: string | null;
  sent_at: string;
  /** #2127: the sender's link preview, re-checked against `content` in Rust. */
  link_preview?: LinkPreviewWire | null;
}

const pinQueryKeys = {
  all: ["pins"] as const,
  conversation: (conversationId: string | null) =>
    ["pins", conversationId] as const,
};

/** One conversation's pins, newest first; may error until the pin key arrives. */
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
  /** The local row, drawn as the pin until the list is re-read (#2141). */
  source?: Pick<PinnedMessage, "content" | "sender_id" | "sender_username" | "sender_display_name" | "sent_at"> | null;
}

/** The pinned list as it reads once the toggle lands, drawn on the tap (#2141). */
function togglePinLocally(
  pins: PinnedMessage[],
  { conversationId, messageId, isPinned, source }: TogglePinVars,
  pinnedBy: string,
): PinnedMessage[] {
  if (isPinned) {
    return pins.filter((p) => p.message_id !== messageId);
  }
  if (!source || pins.some((p) => p.message_id === messageId)) {
    return pins;
  }
  return [
    {
      id: `pending-pin-${messageId}`,
      conversation_id: conversationId,
      message_id: messageId,
      pinned_by: pinnedBy,
      pinned_at: new Date().toISOString(),
      ...source,
      link_preview: null,
    },
    ...pins,
  ];
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
    // Drawn on the tap (#2141), rolled back if the core refuses, re-read either way.
    onMutate: async (vars) => {
      const key = pinQueryKeys.conversation(vars.conversationId);
      await queryClient.cancelQueries({ queryKey: key });
      const previous = queryClient.getQueryData<PinnedMessage[]>(key);
      queryClient.setQueryData<PinnedMessage[]>(key, (pins) =>
        togglePinLocally(pins ?? [], vars, currentUser?.id ?? ""),
      );
      return { previous, key };
    },
    onError: (_err, _vars, context) => {
      if (context) {
        queryClient.setQueryData(context.key, context.previous);
      }
    },
    onSettled: (_data, _err, { conversationId }) => {
      void queryClient.invalidateQueries({
        queryKey: pinQueryKeys.conversation(conversationId),
      });
    },
  });
}
