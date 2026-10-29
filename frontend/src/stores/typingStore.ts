import { makeAutoObservable } from "mobx";

// Receiver-side TTL: an unrefreshed entry ages out even without `is_typing:
// false` (sender dropped offline).
export const TYPING_TTL_MS = 6000;

// Sender re-emit cadence; must be < TYPING_TTL_MS.
export const TYPING_REFRESH_MS = 3000;

export type TypingRoomKey = `channel:${string}` | `dm:${string}`;

interface TypingEntry {
  username: string;
}

// Expiry timers by `${roomKey}:${userId}`, outside the observable store: not UI state.
const expiryTimers = new Map<string, ReturnType<typeof setTimeout>>();

function timerKey(roomKey: TypingRoomKey, userId: string): string {
  return `${roomKey}:${userId}`;
}

// Drop one user from a room map (and the room when empty); same reference if
// nothing changed.
function removeEntry(
  byRoom: Record<string, Record<string, TypingEntry>>,
  roomKey: string,
  userId: string,
): Record<string, Record<string, TypingEntry>> {
  const room = byRoom[roomKey];
  if (!room || !(userId in room)) {
    return byRoom;
  }
  const next = { ...room };
  delete next[userId];
  const result = { ...byRoom };
  if (Object.keys(next).length === 0) {
    delete result[roomKey];
  } else {
    result[roomKey] = next;
  }
  return result;
}

class TypingStore {
  // roomKey → userId → entry.
  byRoom: Record<string, Record<string, TypingEntry>> = {};

  constructor() {
    makeAutoObservable(this, {}, { autoBind: true });
  }

  setTyping(roomKey: TypingRoomKey, userId: string, username: string) {
    const room = { ...(this.byRoom[roomKey] ?? {}) };
    room[userId] = { username };
    this.byRoom = { ...this.byRoom, [roomKey]: room };

    // (Re)arm the expiry timer; a refresh resets the countdown.
    const key = timerKey(roomKey, userId);
    const existing = expiryTimers.get(key);
    if (existing) {
      clearTimeout(existing);
    }
    expiryTimers.set(
      key,
      setTimeout(() => {
        expiryTimers.delete(key);
        this.byRoom = removeEntry(this.byRoom, roomKey, userId);
      }, TYPING_TTL_MS),
    );
  }

  clearTyping(roomKey: TypingRoomKey, userId: string) {
    const key = timerKey(roomKey, userId);
    const existing = expiryTimers.get(key);
    if (existing) {
      clearTimeout(existing);
      expiryTimers.delete(key);
    }
    this.byRoom = removeEntry(this.byRoom, roomKey, userId);
  }
}

export const typingStore = new TypingStore();

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
