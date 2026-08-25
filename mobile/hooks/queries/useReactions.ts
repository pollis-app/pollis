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
import { applyReactionToggle } from "./reactionOptimistic";
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
 *
 * **Optimistic.** The pill moves on the tap, not on the round trip. The
 * outcome of a reaction tap is fully known client-side — it adds you to that
 * emoji or takes you off it — so waiting for `add_reaction` and then for the
 * refetch it invalidates made a decided gesture feel unacknowledged twice
 * over. `applyReactionToggle` computes exactly what the query will report, so
 * the refetch that eventually replaces this confirms it rather than correcting
 * it, and nothing visibly settles.
 *
 * The write is still the authority: a failure restores the snapshot taken
 * before the tap, and `onSettled` invalidates either way so the server's answer
 * is what survives.
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

    onMutate: async (vars) => {
      if (!currentUser || !conversationId || !kind) {
        return { previous: [] };
      }
      // The live key carries the loaded-id list as a final segment, so match on
      // the conversation PREFIX — otherwise the entry actually on screen is the
      // one entry left un-updated.
      const queryKey = reactionQueryKeys.conversation(kind, conversationId);

      // An in-flight read would land after this write and overwrite it with
      // pre-tap data.
      await queryClient.cancelQueries({ queryKey });

      const previous = queryClient.getQueriesData<Map<string, Reaction[]>>({
        queryKey,
      });
      queryClient.setQueriesData<Map<string, Reaction[]>>(
        { queryKey },
        (old) =>
          old === undefined
            ? old
            : applyReactionToggle(old, { ...vars, userId: currentUser.id }),
      );
      return { previous };
    },

    onError: (_error, _vars, context) => {
      for (const [key, data] of context?.previous ?? []) {
        queryClient.setQueryData(key, data);
      }
    },

    // On both paths, not just success: a rolled-back failure still needs the
    // server's version of the truth, and a success needs the reaction another
    // device may have added in the meantime.
    onSettled: () => {
      if (conversationId && kind) {
        queryClient.invalidateQueries({
          queryKey: reactionQueryKeys.conversation(kind, conversationId),
        });
      }
    },
  });
}
