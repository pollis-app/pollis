import { makeAutoObservable } from "mobx";

// Presence from LiveKit room participation: online while any shared room shows
// the user, tracked per room so leaving one doesn't flip them offline.

export type PresenceStatus = "online" | "offline";

class PresenceStore {
  byUser: Record<string, Set<string>> = {};

  // Realtime events only describe OTHER participants, so without this the local
  // user reads offline. Set from `appStore.setCurrentUser`.
  selfUserId: string | null = null;

  constructor() {
    // `isOnline` stays a plain method, not an action: actions run untracked, so
    // an observer wouldn't track its `byUser` reads.
    makeAutoObservable(this, { isOnline: false }, { autoBind: true });
  }

  // A setter, so this store imports nothing and the dependency stays one-way.
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

  // Before `realtime_reconnected` re-emits the participant snapshot.
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
    // If the app is running, we are online.
    if (userId === this.selfUserId) {
      return true;
    }
    return (this.byUser[userId]?.size ?? 0) > 0;
  }
}

export const presenceStore = new PresenceStore();

/** "online" if this user is in any shared room; call from an `observer()`. */
export function usePresenceStatus(userId: string | null | undefined): PresenceStatus {
  return presenceStore.isOnline(userId) ? "online" : "offline";
}
