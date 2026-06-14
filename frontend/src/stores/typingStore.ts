import { makeAutoObservable } from "mobx";

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
}

// Per-entry expiry timer handles, keyed by `${roomKey}:${userId}`. These live
// OUTSIDE the observable store on purpose — a timer handle is not UI state, so
// it must never be tracked by MobX or trigger a re-render.
const expiryTimers = new Map<string, ReturnType<typeof setTimeout>>();

function timerKey(roomKey: TypingRoomKey, userId: string): string {
  return `${roomKey}:${userId}`;
}

// Immutably drop a single user from a room map, removing the room key itself
// when it empties. Pure: takes the current byRoom and returns the next one
// (the same reference when there was nothing to remove).
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
  // roomKey → userId → entry. Two-level map so typing in one channel doesn't
  // leak into another and lookups stay O(1).
  byRoom: Record<string, Record<string, TypingEntry>> = {};

  constructor() {
    makeAutoObservable(this, {}, { autoBind: true });
  }

  setTyping(roomKey: TypingRoomKey, userId: string, username: string) {
    const room = { ...(this.byRoom[roomKey] ?? {}) };
    room[userId] = { username };
    this.byRoom = { ...this.byRoom, [roomKey]: room };

    // (Re)arm a per-entry expiry timer. A refresh resets the countdown, so an
    // entry only ages out once TTL elapses with no further keystroke — and a
    // sender that drops offline mid-keystroke is cleaned up exactly once,
    // without any global polling.
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
