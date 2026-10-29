// Pinned messages (#1315), a port of desktop's usePins.ts. DS-backed, sealed under the conversation's pin key.

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

/** One conversation's pins, newest first. May error until the pin key arrives. `fresh`: pins have no realtime event. */
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

export function usePinnedMessageIds(conversationId: string | null): Set<string> {
  const { data } = usePinnedMessages(conversationId);
  return useMemo(() => pinnedIds(data ?? []), [data]);
}

export type TogglePinVars = {
  conversationId: string;
  messageId: string;
  /** `true` means this toggle UNPINS. */
  isPinned: boolean;
};

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
