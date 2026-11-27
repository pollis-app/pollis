// Reaction hooks: ONE query per conversation, read in one batched call (no per-row observers, #874/#936, #2115).

import { useEffect, useRef, useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { invoke } from "../../lib/native";
import { applyReactionToggle } from "./reactionOptimistic";
import { noteLocalReaction } from "../../lib/reactionMotion";
import {
  mayReveal,
  mergeReactions,
  missingIds,
  readReactions,
  REACTIONS_REVEAL_WAIT_MS,
  stillCurrent,
  type ReactionMap,
} from "./reactionBatch";
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
 * Reactions per loaded message, keyed by id, one map per conversation (#2115). A refresh
 * re-reads every loaded id in ONE `get_reactions_for_messages`; a new page or message reads
 * only the ids the map lacks and merges them in. The map outlives the screen in the cache,
 * so reopening a conversation paints its pills with the first frame.
 */
export function useConversationReactions(
  conversationId: string | null,
  kind: ConversationKind | null,
  messageIds: readonly string[],
) {
  const queryClient = useQueryClient();
  const queryKey = reactionQueryKeys.conversation(kind, conversationId);
  const enabled = !!(conversationId && kind);
  // The queryFn reads the ids loaded when it runs, not when it was created.
  const idsRef = useRef(messageIds);
  idsRef.current = messageIds;

  const query = useQuery({
    queryKey,
    queryFn: async (): Promise<ReactionMap> => {
      const fresh = await readReactions(idsRef.current, invoke);
      return mergeReactions(queryClient.getQueryData<ReactionMap>(queryKey), fresh);
    },
    enabled,
    staleTime: 1000 * 15,
  });

  // Ids already being read, so a burst of renders asks for each once.
  const inFlight = useRef(new Set<string>());
  const idsKey = messageIds.join("|");
  const hasData = query.data !== undefined;
  useEffect(() => {
    // The first read covers every id loaded when it runs.
    if (!enabled || !hasData) {
      return;
    }
    const missing = missingIds(query.data, messageIds).filter(
      (id) => !inFlight.current.has(id),
    );
    if (missing.length === 0) {
      return;
    }
    for (const id of missing) {
      inFlight.current.add(id);
    }
    readReactions(missing, invoke)
      .then((fresh) => {
        queryClient.setQueryData<ReactionMap>(queryKey, (old) => mergeReactions(old, fresh));
      })
      .catch(() => {
        // Left missing: the next change or refresh asks again.
      })
      .finally(() => {
        for (const id of missing) {
          inFlight.current.delete(id);
        }
      });
    // idsKey stands for messageIds; query.data is read fresh on each run.
  }, [enabled, hasData, idsKey, conversationId, kind]);

  return query;
}

/**
 * Holds a freshly opened conversation's list back until its reactions are known, at most
 * `REACTIONS_REVEAL_WAIT_MS`, so pills paint with their rows instead of pushing them (#2115).
 * A reopened conversation has its map cached and shows at once.
 */
export function useRevealWithReactions(
  conversationKey: string,
  reactions: ReadonlyMap<string, Reaction[]> | undefined,
  messageIds: readonly string[],
): boolean {
  const [timedOutFor, setTimedOutFor] = useState<string | null>(null);
  const shownFor = useRef<string | null>(null);
  const hasIds = messageIds.length > 0;
  const shown = shownFor.current === conversationKey;

  useEffect(() => {
    if (!hasIds || shown) {
      return;
    }
    const timer = setTimeout(() => setTimedOutFor(conversationKey), REACTIONS_REVEAL_WAIT_MS);
    return () => clearTimeout(timer);
  }, [hasIds, shown, conversationKey]);

  if (shown) {
    return true;
  }
  if (!mayReveal(reactions, messageIds, timedOutFor === conversationKey)) {
    return false;
  }
  if (hasIds) {
    shownFor.current = conversationKey;
  }
  return true;
}

/** How long a burst of taps waits before its one re-read (#2141). */
const REREAD_COALESCE_MS = 250;

/** Each message's toggle count, so a re-read never overwrites a later tap (`stillCurrent`). */
const toggleCounts = new Map<string, number>();

/** Toggled ids waiting for their re-read, per conversation's reaction key. */
const rereads = new Map<string, { key: readonly unknown[]; ids: Set<string> }>();

/**
 * After a toggle, re-read only the toggled messages (#2141), several taps in one call. The
 * conversation's other rows keep their entries, so no other pill re-renders.
 */
function rereadToggled(
  queryClient: ReturnType<typeof useQueryClient>,
  key: readonly unknown[],
  messageId: string,
) {
  const slot = JSON.stringify(key);
  const pending = rereads.get(slot);
  if (pending) {
    pending.ids.add(messageId);
    return;
  }
  rereads.set(slot, { key, ids: new Set([messageId]) });
  setTimeout(() => {
    const taken = rereads.get(slot);
    rereads.delete(slot);
    if (!taken) {
      return;
    }
    const ids = [...taken.ids];
    const asked = new Map(ids.map((id) => [id, toggleCounts.get(id) ?? 0]));
    readReactions(ids, invoke)
      .then((fresh) => {
        const current = stillCurrent(fresh, asked, toggleCounts);
        if (current.size === 0) {
          return;
        }
        queryClient.setQueriesData<ReactionMap>({ queryKey: taken.key }, (old) =>
          old === undefined ? old : mergeReactions(old, current),
        );
      })
      .catch(() => {
        // Left as drawn: the conversation's next refresh reads it again.
      });
  }, REREAD_COALESCE_MS);
}

/** Toggle a reaction, optimistically: `applyReactionToggle` computes what the re-read will report; failure restores the snapshot. */
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
      toggleCounts.set(vars.messageId, (toggleCounts.get(vars.messageId) ?? 0) + 1);
      // The pill this adds pops in, even on a row that had none (#2125).
      if (vars.mode === "add") {
        noteLocalReaction(vars.messageId, vars.emoji);
      }
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
    // Only the toggled message is re-read, never every loaded one (#2141).
    onSettled: (_data, _error, vars) => {
      if (conversationId && kind) {
        rereadToggled(queryClient, reactionQueryKeys.conversation(kind, conversationId), vars.messageId);
      }
    },
  });
}
