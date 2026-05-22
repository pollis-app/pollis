import { useTypingStore, typingRoomKey } from "../stores/typingStore";

/**
 * Returns the list of usernames currently typing in the named room, sorted
 * for stable display. Entries age out automatically — each `setTyping` call
 * in the store schedules a single setTimeout at the entry's expiry, so this
 * hook is a pure selector with no effects, no timers, and no per-mount cost.
 */
export function useTypingState(args: {
  channelId: string | null;
  conversationId: string | null;
}): string[] {
  const { channelId, conversationId } = args;
  const roomKey = typingRoomKey(channelId, conversationId);

  const room = useTypingStore((s) => (roomKey ? s.byRoom[roomKey] : undefined));

  if (!room) {
    return [];
  }
  return Object.values(room)
    .map((entry) => entry.username)
    .sort((a, b) => a.localeCompare(b));
}
