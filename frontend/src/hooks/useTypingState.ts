import { useObserver } from "mobx-react-lite";
import { typingStore, typingRoomKey } from "../stores/typingStore";
import { useSelectedGroupAliases } from "./queries/useGroupAliases";
import { aliasName } from "../utils/groupAlias";

/** Sorted names typing in the room: in a channel, as its group shows them (#2003). */
export function useTypingState(args: {
  channelId: string | null;
  conversationId: string | null;
}): string[] {
  const { channelId, conversationId } = args;
  const roomKey = typingRoomKey(channelId, conversationId);

  const room = useObserver(() => (roomKey ? typingStore.byRoom[roomKey] : undefined));
  const aliases = useSelectedGroupAliases();

  if (!room) {
    return [];
  }
  return Object.entries(room)
    .map(([userId, entry]) => (channelId ? aliasName(aliases, userId, entry.username) : entry.username))
    .sort((a, b) => a.localeCompare(b));
}
