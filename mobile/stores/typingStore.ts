// Who is typing where (#1635), in memory only (desktop's twin). Entries age out on a TTL.

import { makeAutoObservable } from "mobx";
import { TYPING_TTL_MS, type TypingRoomKey } from "../lib/typing.ts";

type TypingEntry = { username: string };

// Timer handles live outside the observable store.
const expiryTimers = new Map<string, ReturnType<typeof setTimeout>>();

function timerKey(roomKey: TypingRoomKey, userId: string): string {
  return `${roomKey}:${userId}`;
}

class TypingStore {
  byRoom: Record<string, Record<string, TypingEntry>> = {};

  constructor() {
    makeAutoObservable(this, {}, { autoBind: true });
  }

  setTyping(roomKey: TypingRoomKey, userId: string, username: string) {
    this.byRoom = {
      ...this.byRoom,
      [roomKey]: { ...(this.byRoom[roomKey] ?? {}), [userId]: { username } },
    };
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
        this.clearTyping(roomKey, userId);
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
    const room = this.byRoom[roomKey];
    if (!room || !(userId in room)) {
      return;
    }
    const nextRoom = { ...room };
    delete nextRoom[userId];
    const next = { ...this.byRoom };
    if (Object.keys(nextRoom).length === 0) {
      delete next[roomKey];
    } else {
      next[roomKey] = nextRoom;
    }
    this.byRoom = next;
  }

  /** Sorted usernames typing in the room. */
  typists(roomKey: TypingRoomKey | null): string[] {
    const room = roomKey ? this.byRoom[roomKey] : undefined;
    if (!room) {
      return [];
    }
    return Object.values(room)
      .map((e) => e.username)
      .sort((a, b) => a.localeCompare(b));
  }

  clear() {
    for (const t of expiryTimers.values()) {
      clearTimeout(t);
    }
    expiryTimers.clear();
    this.byRoom = {};
  }
}

export const typingStore = new TypingStore();
