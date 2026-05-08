import { useEffect } from "react";
import { useTypingStore, TYPING_TTL_MS, typingRoomKey } from "../stores/typingStore";

/**
 * Returns the list of usernames currently typing in the named room, sorted
 * for stable display. The store is fed by `useLiveKitRealtime`'s typing
 * dispatcher; this hook layers a periodic prune so entries age out when
 * the sender drops offline without an explicit clear.
 */
export function useTypingState(args: {
  channelId: string | null;
  conversationId: string | null;
}): string[] {
  const { channelId, conversationId } = args;
  const roomKey = typingRoomKey(channelId, conversationId);

  const room = useTypingStore((s) => (roomKey ? s.byRoom[roomKey] : undefined));
  const pruneExpired = useTypingStore((s) => s.pruneExpired);

  // Drive the prune on a coarse interval — finer than TTL but cheap. The
  // store no-ops when nothing actually expired.
  useEffect(() => {
    const id = setInterval(pruneExpired, Math.min(1000, TYPING_TTL_MS / 4));
    return () => clearInterval(id);
  }, [pruneExpired]);

  if (!room) {
    return [];
  }
  const now = Date.now();
  return Object.values(room)
    .filter((entry) => entry.expiresAt > now)
    .map((entry) => entry.username)
    .sort((a, b) => a.localeCompare(b));
}
