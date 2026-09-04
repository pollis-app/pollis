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

/** The cached list with `userId` added to `emoji` — a new pill if nobody had
 * reacted with it yet. Pure: never mutates the cached array. */
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

/** The cached list with `userId` taken off `emoji`; a pill nobody is left on
 * disappears, so an empty list means the row unmounts. */
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
 * Optimistic reaction write. The pill flips the instant the emoji is picked,
 * ahead of the round trip — a reaction that lands a second later reads as a
 * broken click. The mutation still owns the truth: on error the previous
 * list is put back, and either way the query is refetched once the write
 * settles so the DS's answer wins.
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
      // A refetch already in flight would overwrite the optimistic list with
      // the pre-write answer; stop it.
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
 * Toggle the current user's reaction on one message: remove it if they
 * already reacted with that emoji, add it otherwise. Shared by the reaction
 * pills and the hover-bar picker so both agree on what a second click means.
 *
 * Decides from the cache at click time rather than from the last render's
 * list, so two quick clicks on the same emoji read the optimistic result of
 * the first and take the reaction back instead of adding it twice.
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
