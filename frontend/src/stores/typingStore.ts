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
  // Drop expired entries; called from a polling effect so the UI re-renders
  // when a typing indicator times out without an explicit clear.
  pruneExpired: () => void;
}

export const useTypingStore = create<TypingStore>((set) => ({
  byRoom: {},
  setTyping: (roomKey, userId, username) => {
    set((state) => {
      const room = { ...(state.byRoom[roomKey] ?? {}) };
      room[userId] = { username, expiresAt: Date.now() + TYPING_TTL_MS };
      return { byRoom: { ...state.byRoom, [roomKey]: room } };
    });
  },
  clearTyping: (roomKey, userId) => {
    set((state) => {
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
    });
  },
  pruneExpired: () => {
    set((state) => {
      const now = Date.now();
      let changed = false;
      const byRoom: Record<string, Record<string, TypingEntry>> = {};
      for (const [roomKey, room] of Object.entries(state.byRoom)) {
        const live: Record<string, TypingEntry> = {};
        for (const [userId, entry] of Object.entries(room)) {
          if (entry.expiresAt > now) {
            live[userId] = entry;
          } else {
            changed = true;
          }
        }
        if (Object.keys(live).length > 0) {
          byRoom[roomKey] = live;
        } else if (Object.keys(room).length > 0) {
          changed = true;
        }
      }
      return changed ? { byRoom } : state;
    });
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
