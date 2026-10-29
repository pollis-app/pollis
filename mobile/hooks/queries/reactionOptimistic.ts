import type { Reaction } from "./useReactions";

export interface ToggleIntent {
  messageId: string;
  emoji: string;
  userId: string;
  mode: "add" | "remove";
}

/**
 * Apply a toggle to a conversation's reaction map, returning exactly what
 * `useConversationReactions` will report after the write (sorted pills, no
 * empty entries), so the refetch confirms rather than corrects it. A toggle
 * that is already true returns the same map, so a double tap cannot double-count.
 */
export function applyReactionToggle(
  previous: ReadonlyMap<string, Reaction[]>,
  { messageId, emoji, userId, mode }: ToggleIntent,
): Map<string, Reaction[]> {
  const current = previous.get(messageId) ?? [];
  const existing = current.find((r) => r.emoji === emoji);
  const reacted = existing?.user_ids.includes(userId) ?? false;

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
