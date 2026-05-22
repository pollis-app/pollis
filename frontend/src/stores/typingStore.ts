import { create } from "zustand";

// Receiver-side TTL: a typing entry that hasn't been refreshed within this
// many ms ages out, even without an explicit `is_typing: false`. Covers the
// case where a sender drops offline mid-keystroke.
export const TYPING_TTL_MS = 6000;

// Re-emit cadence on the sender side. Pick something < TYPING_TTL_MS so an
// actively-typing user keeps refreshing their own entry.
export const TYPING_REFRESH_MS = 3000;

export type TypingRoomKey = `channel:${string}` | `dm:${string}`;

interface TypingEntry {
  username: string;
  expiresAt: number;
}

interface TypingStore {
  // roomKey → userId → entry. Two-level map so typing in one channel doesn't
  // leak into another and lookups stay O(1).
  byRoom: Record<string, Record<string, TypingEntry>>;
  setTyping: (roomKey: TypingRoomKey, userId: string, username: string) => void;
  clearTyping: (roomKey: TypingRoomKey, userId: string) => void;
}

// Per-entry expiry timers, keyed by `${roomKey}|${userId}`. Lives outside
// Zustand state because timer handles aren't UI-relevant — they're internal
// scheduling. Replacing the periodic prune with one timeout per active entry
// means the cost scales with the number of typers, not with mount count.
const expiryTimers = new Map<string, ReturnType<typeof setTimeout>>();

function timerKey(roomKey: TypingRoomKey, userId: string): string {
  return `${roomKey}|${userId}`;
}

function removeEntry(
  state: { byRoom: Record<string, Record<string, TypingEntry>> },
  roomKey: TypingRoomKey,
  userId: string,
): { byRoom: Record<string, Record<string, TypingEntry>> } {
  const room = state.byRoom[roomKey];
  if (!room || !(userId in room)) {
    return state;
  }
  const next = { ...room };
  delete next[userId];
  const byRoom = { ...state.byRoom };
  if (Object.keys(next).length === 0) {
    delete byRoom[roomKey];
  } else {
    byRoom[roomKey] = next;
  }
  return { byRoom };
}

export const useTypingStore = create<TypingStore>((set) => ({
  byRoom: {},
  setTyping: (roomKey, userId, username) => {
    const key = timerKey(roomKey, userId);
    const existing = expiryTimers.get(key);
    if (existing) {
      clearTimeout(existing);
    }
    expiryTimers.set(
      key,
      setTimeout(() => {
        expiryTimers.delete(key);
        set((state) => removeEntry(state, roomKey, userId));
      }, TYPING_TTL_MS),
    );
    set((state) => {
      const room = { ...(state.byRoom[roomKey] ?? {}) };
      room[userId] = { username, expiresAt: Date.now() + TYPING_TTL_MS };
      return { byRoom: { ...state.byRoom, [roomKey]: room } };
    });
  },
  clearTyping: (roomKey, userId) => {
    const key = timerKey(roomKey, userId);
    const t = expiryTimers.get(key);
    if (t) {
      clearTimeout(t);
      expiryTimers.delete(key);
    }
    set((state) => removeEntry(state, roomKey, userId));
  },
}));

export function typingRoomKey(
  channelId: string | null | undefined,
  conversationId: string | null | undefined,
): TypingRoomKey | null {
  if (channelId) {
    return `channel:${channelId}`;
  }
  if (conversationId) {
    return `dm:${conversationId}`;
  }
  return null;
}
