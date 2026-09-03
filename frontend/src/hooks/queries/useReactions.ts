import { useQuery, useMutation, useQueryClient } from "@tanstack/react-query";
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

export function useAddReaction() {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: async ({
      messageId,
      userId,
      emoji,
    }: {
      messageId: string;
      userId: string;
      emoji: string;
    }) => {
      await invoke("add_reaction", { messageId, userId, emoji });
    },
    onSuccess: (_data, variables) => {
      queryClient.invalidateQueries({
        queryKey: reactionQueryKeys.message(variables.messageId),
      });
    },
  });
}

export function useRemoveReaction() {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: async ({
      messageId,
      userId,
      emoji,
    }: {
      messageId: string;
      userId: string;
      emoji: string;
    }) => {
      await invoke("remove_reaction", { messageId, userId, emoji });
    },
    onSuccess: (_data, variables) => {
      queryClient.invalidateQueries({
        queryKey: reactionQueryKeys.message(variables.messageId),
      });
    },
  });
}

/**
 * Toggle the current user's reaction on one message: remove it if they
 * already reacted with that emoji, add it otherwise. Shared by the reaction
 * pills and the hover-bar picker so both agree on what a second click means.
 */
export function useToggleReaction(messageId: string) {
  const { data: reactions = [] } = useReactions(messageId);
  const addReaction = useAddReaction();
  const removeReaction = useRemoveReaction();
  const { mutate: add } = addReaction;
  const { mutate: remove } = removeReaction;

  return useCallback(
    (emoji: string) => {
      const currentUser = appStore.currentUser;
      if (!currentUser) {
        return;
      }
      const existing = reactions.find((r) => r.emoji === emoji);
      const alreadyReacted = existing?.user_ids.includes(currentUser.id) ?? false;
      if (alreadyReacted) {
        remove({ messageId, userId: currentUser.id, emoji });
      } else {
        add({ messageId, userId: currentUser.id, emoji });
      }
    },
    [messageId, reactions, add, remove],
  );
}
