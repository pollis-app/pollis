import { useObserver } from "mobx-react-lite";
import { typingStore, typingRoomKey } from "../stores/typingStore";

/** Sorted usernames typing in the room. */
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
