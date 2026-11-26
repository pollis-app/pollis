// Who is online or away (#2059, #2110), desktop's twin (frontend/src/stores/presenceStore.ts): online
// while any room this device shares with them shows them in use, away while every room that shows
// them shows them not in use. The input is per-room snapshots only
// (`setRoom` / `clearRoom`), so a different presence source can feed it without touching readers.

import { comparer, computed, makeAutoObservable } from "mobx";
import {
  presentUsers,
  statusOf,
  withRoom,
  withoutRoom,
  type PresenceStatus,
  type RoomPresence,
  type RoomSnapshot,
} from "../lib/presence.ts";

class PresenceStore {
  byRoom: RoomPresence = {};

  // The rooms never report this device's own user, so self is set from `appStore.setCurrentUser`.
  selfUserId: string | null = null;

  constructor() {
    // `status` stays tracked: as an action its reads would be untracked.
    // `present` compares by content, so a snapshot that changes no one re-renders no one.
    makeAutoObservable(
      this,
      { status: false, present: computed({ equals: comparer.structural }) },
      { autoBind: true },
    );
  }

  // Computed, so a status read is a map lookup: user → in use somewhere.
  get present(): Map<string, boolean> {
    return presentUsers(this.byRoom);
  }

  setSelfUserId(userId: string | null) {
    this.selfUserId = userId;
  }

  setRoom(roomId: string, snapshot: RoomSnapshot) {
    this.byRoom = withRoom(this.byRoom, roomId, snapshot);
  }

  clearRoom(roomId: string) {
    this.byRoom = withoutRoom(this.byRoom, roomId);
  }

  reset() {
    this.byRoom = {};
    this.selfUserId = null;
  }

  status(userId: string | null | undefined): PresenceStatus {
    return statusOf(this.present, userId, this.selfUserId);
  }
}

export const presenceStore = new PresenceStore();
