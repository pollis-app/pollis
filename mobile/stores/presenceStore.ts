// Who is online (#2059), desktop's twin (frontend/src/stores/presenceStore.ts): online while any
// room this device shares with them shows them. The input is per-room snapshots only
// (`setRoom` / `clearRoom`), so a different presence source can feed it without touching readers.

import { comparer, computed, makeAutoObservable } from "mobx";
import {
  onlineUsers,
  statusOf,
  withRoom,
  withoutRoom,
  type PresenceStatus,
  type RoomPresence,
} from "../lib/presence.ts";

export type { PresenceStatus };

class PresenceStore {
  byRoom: RoomPresence = {};

  // The rooms never report this device's own user, so self is set from `appStore.setCurrentUser`.
  selfUserId: string | null = null;

  constructor() {
    // `status` stays tracked: as an action its reads would be untracked.
    // `online` compares by content, so a snapshot that changes no one re-renders no one.
    makeAutoObservable(
      this,
      { status: false, online: computed({ equals: comparer.structural }) },
      { autoBind: true },
    );
  }

  // Computed, so a status read is a set lookup.
  get online(): Set<string> {
    return onlineUsers(this.byRoom);
  }

  setSelfUserId(userId: string | null) {
    this.selfUserId = userId;
  }

  setRoom(roomId: string, userIds: string[]) {
    this.byRoom = withRoom(this.byRoom, roomId, userIds);
  }

  clearRoom(roomId: string) {
    this.byRoom = withoutRoom(this.byRoom, roomId);
  }

  reset() {
    this.byRoom = {};
    this.selfUserId = null;
  }

  status(userId: string | null | undefined): PresenceStatus {
    return statusOf(this.online, userId, this.selfUserId);
  }
}

export const presenceStore = new PresenceStore();
