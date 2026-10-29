import { makeAutoObservable } from "mobx";

// Online while any shared LiveKit room shows the user, tracked per room.

export type PresenceStatus = "online" | "offline";

class PresenceStore {
  // user_id → Set<room_id>
  byUser: Record<string, Set<string>> = {};

  // We never get room events about ourselves, so self is set from `appStore.setCurrentUser`.
  selfUserId: string | null = null;

  constructor() {
    // `isOnline` stays tracked: as an action its reads would be untracked.
    makeAutoObservable(this, { isOnline: false }, { autoBind: true });
  }

  // A setter keeps this store import-free.
  setSelfUserId(userId: string | null) {
    this.selfUserId = userId;
  }

  setPresent(userId: string, roomId: string, present: boolean) {
    const existing = this.byUser[userId];
    const nextSet = new Set(existing ?? []);
    if (present) {
      nextSet.add(roomId);
    } else {
      nextSet.delete(roomId);
    }
    const byUser = { ...this.byUser };
    if (nextSet.size === 0) {
      delete byUser[userId];
    } else {
      byUser[userId] = nextSet;
    }
    this.byUser = byUser;
  }

  resetRoom(roomId: string) {
    let changed = false;
    const byUser: Record<string, Set<string>> = {};
    for (const [userId, rooms] of Object.entries(this.byUser)) {
      if (!rooms.has(roomId)) {
        byUser[userId] = rooms;
        continue;
      }
      const next = new Set(rooms);
      next.delete(roomId);
      if (next.size > 0) {
        byUser[userId] = next;
      }
      changed = true;
    }
    if (changed) {
      this.byUser = byUser;
    }
  }

  isOnline(userId: string | null | undefined): boolean {
    if (!userId) {
      return false;
    }
    if (userId === this.selfUserId) {
      return true;
    }
    return (this.byUser[userId]?.size ?? 0) > 0;
  }
}

export const presenceStore = new PresenceStore();

/** "online" if seen in any shared room; call inside `observer()`. */
export function usePresenceStatus(userId: string | null | undefined): PresenceStatus {
  return presenceStore.isOnline(userId) ? "online" : "offline";
}
