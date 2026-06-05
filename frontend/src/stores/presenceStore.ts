import { makeAutoObservable } from "mobx";

// Presence is inferred from LiveKit room participation: a user is online as
// long as at least one room they share with us shows them as connected. The
// store tracks the per-user → set-of-rooms map so leaving one room doesn't
// flip them offline if they're still in another shared room.

export type PresenceStatus = "online" | "offline";

class PresenceStore {
  // user_id → Set<room_id>
  byUser: Record<string, Set<string>> = {};

  constructor() {
    // `isOnline` is excluded from auto-annotation (left a plain method, not an
    // `action`). Actions run in an untracked context, so if `isOnline` were an
    // action its `byUser` reads would not be tracked by the observing
    // component and presence wouldn't update live. As a plain method its reads
    // are tracked by the enclosing observer's render.
    makeAutoObservable(this, { isOnline: false }, { autoBind: true });
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

  // Drop every record for the named room — used on `realtime_reconnected`
  // before re-emitting the new participant snapshot.
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
    return userId ? (this.byUser[userId]?.size ?? 0) > 0 : false;
  }
}

export const presenceStore = new PresenceStore();

/**
 * Lightweight derivation — returns "online" if we currently see this user in
 * any shared room, otherwise "offline". Designed to be called from the
 * components that render avatars (DM list, search panel, profile page). The
 * caller must be wrapped in `observer()` for the value to stay live.
 */
export function usePresenceStatus(userId: string | null | undefined): PresenceStatus {
  return presenceStore.isOnline(userId) ? "online" : "offline";
}
