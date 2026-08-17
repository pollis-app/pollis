// Reaction read hooks. `get_reactions` is a local SQLite read grouped per
// message (mirror of pollis-core/src/commands/messages/reactions.rs).
//
// Granularity note: desktop's original reactions UI ran one React Query
// observer + one invoke PER MESSAGE ROW, and that pattern was deleted in the
// #874 perf audit (#936). There is no batched core command, so mobile keeps
// ONE query per conversation that fans out the (cheap, local) per-message
// reads inside a single queryFn and returns a map — one observer, no
// per-row cache entries.

import {
  useMutation,
  useQuery,
  useQueryClient,
  keepPreviousData,
} from "@tanstack/react-query";
import { invoke } from "../../lib/native";
import { appStore } from "../../stores/appStore";
import { useObserver } from "mobx-react-lite";
import type { ConversationKind } from "./useMessages";

/** Mirror of `pollis_core::commands::messages::reactions::Reaction`. */
export interface Reaction {
  emoji: string;
  user_ids: string[];
  count: number;
}

export const reactionQueryKeys = {
  all: ["reactions"] as const,
  conversation: (
    kind: ConversationKind | null,
    conversationId: string | null,
  ) => ["reactions", kind, conversationId] as const,
};

/**
 * Reactions for every loaded message in a conversation, keyed by message id.
 * Messages with no reactions are absent from the map. The id list is part of
 * the query key (so newly loaded pages extend the fetch) but
 * `keepPreviousData` holds the last map during the refetch, so pills don't
 * flicker as pages load.
 */
export function useConversationReactions(
  conversationId: string | null,
  kind: ConversationKind | null,
  messageIds: readonly string[],
) {
  // Stable across renders for the same set; changes when pages load.
  const idsKey = messageIds.length > 0 ? messageIds.join("|") : "";
  return useQuery({
    queryKey: [
      ...reactionQueryKeys.conversation(kind, conversationId),
      idsKey,
    ],
    queryFn: async (): Promise<Map<string, Reaction[]>> => {
      const map = new Map<string, Reaction[]>();
      if (messageIds.length === 0) {
        return map;
      }
      const results = await Promise.all(
        messageIds.map(async (messageId) => {
          try {
            const reactions = await invoke<Reaction[]>("get_reactions", {
              messageId,
            });
            return { messageId, reactions: reactions ?? [] };
          } catch {
            // A single failed read must not blank every pill in the list.
            return { messageId, reactions: [] as Reaction[] };
          }
        }),
      );
      for (const { messageId, reactions } of results) {
        if (reactions.length > 0) {
          // The Rust side groups via HashMap, so ordering is
          // non-deterministic — sort for stable pill order.
          map.set(
            messageId,
            [...reactions].sort((a, b) => a.emoji.localeCompare(b.emoji)),
          );
        }
      }
      return map;
    },
    enabled: !!(conversationId && kind),
    placeholderData: keepPreviousData,
    staleTime: 1000 * 15,
  });
}

/**
 * Toggle a reaction. The caller passes the resolved `mode` (derived from
 * whether the current user is already in the pill's `user_ids`); this hook
 * dispatches the intent and refreshes the conversation's reaction map.
 */
export function useToggleReaction(
  conversationId: string | null,
  kind: ConversationKind | null,
) {
  const queryClient = useQueryClient();
  const currentUser = useObserver(() => appStore.currentUser);
  return useMutation({
    mutationFn: async (vars: {
      messageId: string;
      emoji: string;
      mode: "add" | "remove";
    }) => {
      if (!currentUser) {
        throw new Error("No current user");
      }
      const cmd = vars.mode === "add" ? "add_reaction" : "remove_reaction";
      await invoke(cmd, {
        messageId: vars.messageId,
        userId: currentUser.id,
        emoji: vars.emoji,
      });
    },
    onSuccess: () => {
      if (conversationId && kind) {
        queryClient.invalidateQueries({
          queryKey: reactionQueryKeys.conversation(kind, conversationId),
        });
      }
    },
  });
}
