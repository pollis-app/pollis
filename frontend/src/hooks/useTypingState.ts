import { useObserver } from "mobx-react-lite";
import { typingStore, typingRoomKey } from "../stores/typingStore";

/**
 * Returns the list of usernames currently typing in the named room, sorted
 * for stable display. The store is fed by `useLiveKitRealtime`'s typing
 * dispatcher and ages entries out via per-entry expiry timers, so this hook
 * just reflects whatever entries are currently live.
 */
export function useTypingState(args: {
  channelId: string | null;
  conversationId: string | null;
}): string[] {
  const { channelId, conversationId } = args;
  const roomKey = typingRoomKey(channelId, conversationId);

  const room = useObserver(() => (roomKey ? typingStore.byRoom[roomKey] : undefined));

  if (!room) {
    return [];
  }
  return Object.values(room)
    .map((entry) => entry.username)
    .sort((a, b) => a.localeCompare(b));
}
