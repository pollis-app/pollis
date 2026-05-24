import { useQuery, useMutation, useQueryClient } from "@tanstack/react-query";
import { invoke } from "../../bridge";
import type { Reaction } from "../../types";

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
