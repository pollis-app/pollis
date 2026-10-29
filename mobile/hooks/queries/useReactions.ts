// One query per conversation that fans out the cheap local per-message reads
// in a single queryFn, never one observer per row (#874/#936).

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
 * Reactions for every loaded message in a conversation, keyed by message id;
 * messages with none are absent.
 */
export function useConversationReactions(
  conversationId: string | null,
  kind: ConversationKind | null,
  messageIds: readonly string[],
) {
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
          // The Rust side groups via HashMap, so order is non-deterministic.
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
 * Toggle a reaction optimistically; the caller passes the resolved `mode`. A
 * failure restores the pre-tap snapshot.
 */
export function useToggleReaction(
  conversationId: string | null,
  kind: ConversationKind | null,
) {
  const queryClient = useQueryClient();
  const currentUser = useObserver(() => appStore.currentUser);
  return useMutation({
    meta: { haptic: {
      mutate: (vars: { mode: "add" | "remove" }) =>
        vars.mode === "add" ? "reactionAdd" : "reactionRemove",
    } },
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
      // The live key ends with the loaded-id list, so match on the prefix.
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

    onSettled: () => {
      if (conversationId && kind) {
        queryClient.invalidateQueries({
          queryKey: reactionQueryKeys.conversation(kind, conversationId),
        });
      }
    },
  });
}
