import { makeAutoObservable } from "mobx";

// Presence from LiveKit rooms, tracked per room (#2059, #2110): online while any shared room shows
// the user in use, away while every room that shows them shows them not in use, offline while
// none shows them.

export type PresenceStatus = "online" | "away" | "offline";

/** room_id → whether the user is in use there. */
export type UserRooms = Readonly<Record<string, boolean>>;

/** The rule, pure: no rooms is offline, any room in use is online, else away. */
export function statusFromRooms(rooms: UserRooms | undefined): PresenceStatus {
  const flags = Object.values(rooms ?? {});
  if (flags.length === 0) {
    return "offline";
  }
  return flags.some(Boolean) ? "online" : "away";
}

class PresenceStore {
  // user_id → room_id → in use
  byUser: Record<string, UserRooms> = {};

  // We never get room events about ourselves, so self is set from `appStore.setCurrentUser`.
  selfUserId: string | null = null;

  constructor() {
    // `isPresent` and `status` stay tracked: as actions their reads would be untracked.
    makeAutoObservable(this, { isPresent: false, status: false }, { autoBind: true });
  }

  // A setter keeps this store import-free.
  setSelfUserId(userId: string | null) {
    this.selfUserId = userId;
  }

  /** `active` defaults to in use: an event or caller that predates #2110 means online. */
  setPresent(userId: string, roomId: string, present: boolean, active = true) {
    const next: Record<string, boolean> = { ...(this.byUser[userId] ?? {}) };
    if (present) {
      next[roomId] = active;
    } else {
      delete next[roomId];
    }
    const byUser = { ...this.byUser };
    if (Object.keys(next).length === 0) {
      delete byUser[userId];
    } else {
      byUser[userId] = next;
    }
    this.byUser = byUser;
  }

  resetRoom(roomId: string) {
    let changed = false;
    const byUser: Record<string, UserRooms> = {};
    for (const [userId, rooms] of Object.entries(this.byUser)) {
      if (!(roomId in rooms)) {
        byUser[userId] = rooms;
        continue;
      }
      const next = { ...rooms };
      delete next[roomId];
      if (Object.keys(next).length > 0) {
        byUser[userId] = next;
      }
      changed = true;
    }
    if (changed) {
      this.byUser = byUser;
    }
  }

  /** Online or away: a shared room shows them (self always). */
  isPresent(userId: string | null | undefined): boolean {
    return this.status(userId) !== "offline";
  }

  status(userId: string | null | undefined): PresenceStatus {
    if (!userId) {
      return "offline";
    }
    // You are always online on your own device.
    if (userId === this.selfUserId) {
      return "online";
    }
    return statusFromRooms(this.byUser[userId]);
  }
}

export const presenceStore = new PresenceStore();

/** online / away / offline; call inside `observer()`. */
export function usePresenceStatus(userId: string | null | undefined): PresenceStatus {
  return presenceStore.status(userId);
}
