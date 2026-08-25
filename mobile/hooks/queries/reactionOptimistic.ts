// The reaction map, transformed the way the backend is about to transform it.
//
// `useToggleReaction` used to await `add_reaction`/`remove_reaction` and only
// then invalidate, so a pill did not move until a round trip AND the refetch
// that followed it had both landed. On a local SQLite write that is still two
// async hops for a gesture whose outcome is known the instant it is made —
// tapping 👍 either adds you to that pill or takes you off it.
//
// Kept pure and separate from the hook so the rules can be tested without a
// QueryClient: the tricky parts are not the plumbing but the edges — a pill
// appearing, a pill disappearing, a message leaving the map entirely, and a
// toggle that is already true.

import type { Reaction } from "./useReactions";

export interface ToggleIntent {
  messageId: string;
  emoji: string;
  /** The person doing the toggling — always the local user. */
  userId: string;
  mode: "add" | "remove";
}

/**
 * Apply a toggle to a conversation's reaction map, returning a new map.
 *
 * Mirrors what the query would report after the write, so the optimistic state
 * and the refetch that replaces it agree and nothing visibly settles:
 *
 * - pills are sorted by emoji, because the Rust side groups through a HashMap
 *   and `useConversationReactions` sorts what it gets back;
 * - a message with no reactions is ABSENT from the map rather than holding an
 *   empty array, which is what the `reactions.length > 0` guard in the queryFn
 *   produces;
 * - `count` is derived from `user_ids` rather than adjusted alongside it, so
 *   the two cannot drift apart.
 *
 * A toggle that asks for what is already true returns the map unchanged (by
 * identity), so a double tap cannot double-count.
 */
export function applyReactionToggle(
  previous: ReadonlyMap<string, Reaction[]>,
  { messageId, emoji, userId, mode }: ToggleIntent,
): Map<string, Reaction[]> {
  const current = previous.get(messageId) ?? [];
  const existing = current.find((r) => r.emoji === emoji);
  const reacted = existing?.user_ids.includes(userId) ?? false;

  // Already in the requested state — hand back the same map so React Query
  // sees no change and nothing re-renders.
  if (mode === "add" ? reacted : !reacted) {
    return previous as Map<string, Reaction[]>;
  }

  const others = current.filter((r) => r.emoji !== emoji);
  const userIds =
    mode === "add"
      ? [...(existing?.user_ids ?? []), userId]
      : (existing?.user_ids ?? []).filter((u) => u !== userId);

  const next = new Map(previous);
  if (userIds.length === 0 && others.length === 0) {
    next.delete(messageId);
    return next;
  }

  const pills =
    userIds.length > 0
      ? [...others, { emoji, user_ids: userIds, count: userIds.length }]
      : others;
  next.set(
    messageId,
    pills.sort((a, b) => a.emoji.localeCompare(b.emoji)),
  );
  return next;
}
