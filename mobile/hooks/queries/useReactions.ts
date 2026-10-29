// Reaction hooks: ONE query per conversation fans out the local per-message reads (no per-row observers, #874/#936).

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

/** Reactions per loaded message, keyed by id; `keepPreviousData` stops pills flickering as pages load. */
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
          // Rust groups via HashMap: sort for stable pill order.
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

/** Toggle a reaction, optimistically: `applyReactionToggle` computes what the refetch will report; failure restores the snapshot. */
export function useToggleReaction(
  conversationId: string | null,
  kind: ConversationKind | null,
) {
  const queryClient = useQueryClient();
  const currentUser = useObserver(() => appStore.currentUser);
  return useMutation({
    // Tactile feedback (#1286): lib/queryClient.ts plays it.
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
      // Match on the conversation PREFIX: the live key ends with the loaded-id list.
      const queryKey = reactionQueryKeys.conversation(kind, conversationId);

      // An in-flight read would overwrite this with pre-tap data.
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

    // Both paths: a failure needs the server's truth, a success another device's reactions.
    onSettled: () => {
      if (conversationId && kind) {
        queryClient.invalidateQueries({
          queryKey: reactionQueryKeys.conversation(kind, conversationId),
        });
      }
    },
  });
}
