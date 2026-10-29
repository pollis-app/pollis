import { useQuery, useMutation, useQueryClient, type QueryClient } from "@tanstack/react-query";
import { useCallback } from "react";
import { invoke } from "../../bridge";
import type { Reaction } from "../../types";
import { appStore } from "../../stores/appStore";

export const reactionQueryKeys = {
  message: (messageId: string) => ["reactions", messageId] as const,
};

export function useReactions(messageId: string) {
  return useQuery({
    queryKey: reactionQueryKeys.message(messageId),
    queryFn: async (): Promise<Reaction[]> => {
      return await invoke<Reaction[]>("get_reactions", { messageId });
    },
    staleTime: 1000 * 15,
  });
}

interface ReactionVars {
  messageId: string;
  userId: string;
  emoji: string;
}

/** `userId` added to `emoji` (a new pill if needed), without mutating the cache. */
function withReaction(reactions: Reaction[], userId: string, emoji: string): Reaction[] {
  const existing = reactions.find((r) => r.emoji === emoji);
  if (!existing) {
    return [...reactions, { emoji, user_ids: [userId], count: 1 }];
  }
  if (existing.user_ids.includes(userId)) {
    return reactions;
  }
  return reactions.map((r) =>
    r.emoji === emoji
      ? { ...r, user_ids: [...r.user_ids, userId], count: r.user_ids.length + 1 }
      : r,
  );
}

/** `userId` removed from `emoji`; an empty pill disappears. */
function withoutReaction(reactions: Reaction[], userId: string, emoji: string): Reaction[] {
  return reactions
    .map((r) => {
      if (r.emoji !== emoji) {
        return r;
      }
      const user_ids = r.user_ids.filter((id) => id !== userId);
      return { ...r, user_ids, count: user_ids.length };
    })
    .filter((r) => r.count > 0);
}

/**
 * Optimistic reaction write: the pill flips immediately, rolls back on error,
 * and refetches on settle so the DS's answer wins.
 */
function useOptimisticReactionMutation(
  command: "add_reaction" | "remove_reaction",
  apply: (reactions: Reaction[], userId: string, emoji: string) => Reaction[],
) {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: async ({ messageId, userId, emoji }: ReactionVars) => {
      await invoke(command, { messageId, userId, emoji });
    },
    onMutate: async ({ messageId, userId, emoji }) => {
      const key = reactionQueryKeys.message(messageId);
        // An in-flight refetch would overwrite the optimistic list.
      await queryClient.cancelQueries({ queryKey: key });
      const previous = queryClient.getQueryData<Reaction[]>(key);
      queryClient.setQueryData<Reaction[]>(key, (current = []) =>
        apply(current, userId, emoji),
      );
      return { previous };
    },
    onError: (_err, variables, context) => {
      if (context?.previous !== undefined) {
        queryClient.setQueryData(reactionQueryKeys.message(variables.messageId), context.previous);
      }
    },
    onSettled: (_data, _err, variables) => {
      queryClient.invalidateQueries({
        queryKey: reactionQueryKeys.message(variables.messageId),
      });
    },
  });
}

export function useAddReaction() {
  return useOptimisticReactionMutation("add_reaction", withReaction);
}

export function useRemoveReaction() {
  return useOptimisticReactionMutation("remove_reaction", withoutReaction);
}

/**
 * Toggle the user's reaction on a message, shared by pills and the picker.
 * Decides from the cache at click time, so a quick double click takes it back.
 */
export function useToggleReaction(messageId: string) {
  const queryClient = useQueryClient();
  const { mutate: add } = useAddReaction();
  const { mutate: remove } = useRemoveReaction();

  return useCallback(
    (emoji: string) => {
      const currentUser = appStore.currentUser;
      if (!currentUser) {
        return;
      }
      const reactions = readReactions(queryClient, messageId);
      const existing = reactions.find((r) => r.emoji === emoji);
      const alreadyReacted = existing?.user_ids.includes(currentUser.id) ?? false;
      if (alreadyReacted) {
        remove({ messageId, userId: currentUser.id, emoji });
      } else {
        add({ messageId, userId: currentUser.id, emoji });
      }
    },
    [messageId, queryClient, add, remove],
  );
}

function readReactions(queryClient: QueryClient, messageId: string): Reaction[] {
  return queryClient.getQueryData<Reaction[]>(reactionQueryKeys.message(messageId)) ?? [];
}
