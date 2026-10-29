import { makeAutoObservable } from "mobx";

// Receiver-side TTL, for senders that drop offline mid-keystroke.
export const TYPING_TTL_MS = 6000;

// Sender re-emit cadence; below TYPING_TTL_MS.
export const TYPING_REFRESH_MS = 3000;

export type TypingRoomKey = `channel:${string}` | `dm:${string}`;

interface TypingEntry {
  username: string;
}

// Timer handles live outside the observable store.
const expiryTimers = new Map<string, ReturnType<typeof setTimeout>>();

function timerKey(roomKey: TypingRoomKey, userId: string): string {
  return `${roomKey}:${userId}`;
}

// Drop one user, and the room when empty; same reference if nothing changed.
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
  byRoom: Record<string, Record<string, TypingEntry>> = {};

  constructor() {
    makeAutoObservable(this, {}, { autoBind: true });
  }

  setTyping(roomKey: TypingRoomKey, userId: string, username: string) {
    const room = { ...(this.byRoom[roomKey] ?? {}) };
    room[userId] = { username };
    this.byRoom = { ...this.byRoom, [roomKey]: room };

    // A refresh resets the per-entry countdown.
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
