// Online presence (#2059), desktop's rule: a user is online while any room this device shares with
// them shows them. Pure, so the store and the realtime feeder are tested without a device.

export type PresenceStatus = "online" | "offline";

/** room id → the users that room shows (never this user: the core leaves self out). */
export type RoomPresence = Readonly<Record<string, readonly string[]>>;

/** Replace one room's snapshot; an empty one drops the room. */
export function withRoom(state: RoomPresence, roomId: string, userIds: readonly string[]): RoomPresence {
  const next = { ...state };
  if (userIds.length === 0) {
    delete next[roomId];
  } else {
    next[roomId] = [...new Set(userIds)];
  }
  return next;
}

/** Forget a room this device left: it can no longer vouch for anyone in it. */
export function withoutRoom(state: RoomPresence, roomId: string): RoomPresence {
  if (!(roomId in state)) {
    return state;
  }
  const next = { ...state };
  delete next[roomId];
  return next;
}

/** Everyone some room shows. */
export function onlineUsers(state: RoomPresence): Set<string> {
  const out = new Set<string>();
  for (const users of Object.values(state)) {
    for (const userId of users) {
      out.add(userId);
    }
  }
  return out;
}

/** Self is always online on its own device; the rooms never report it. */
export function statusOf(
  online: ReadonlySet<string>,
  userId: string | null | undefined,
  selfUserId: string | null | undefined,
): PresenceStatus {
  if (!userId) {
    return "offline";
  }
  if (userId === selfUserId || online.has(userId)) {
    return "online";
  }
  return "offline";
}

/**
 * The corner dot's geometry for an avatar `size` points across, desktop's (`ui/Avatar.tsx`): a
 * quarter of the avatar (5 at least), a 2-point ring, sitting 2 points past the corner.
 */
export function presenceDotGeometry(size: number): { dot: number; ring: number; offset: number } {
  return { dot: Math.max(5, Math.round(size * 0.24)), ring: 2, offset: -2 };
}

type Resolve = (roomId: string, identities: string[]) => Promise<string[]>;

export interface PresenceSink {
  setRoom(roomId: string, userIds: string[]): void;
  clearRoom(roomId: string): void;
}

/**
 * Feeds a sink from LiveKit rooms: each refresh hands the core every remote participant's wire
 * identity and stores the users it answers. Only the newest refresh of a room lands, and none
 * lands after the room was dropped. A failed resolve (offline, or a binary without the command)
 * leaves the room as it was.
 */
export function createRoomPresenceFeed(resolve: Resolve, sink: PresenceSink) {
  const generation = new Map<string, number>();

  return {
    refresh(roomId: string, identities: string[]): Promise<void> {
      const gen = (generation.get(roomId) ?? 0) + 1;
      generation.set(roomId, gen);
      return resolve(roomId, identities)
        .then((userIds) => {
          if (generation.get(roomId) !== gen) {
            return;
          }
          sink.setRoom(roomId, Array.isArray(userIds) ? userIds : []);
        })
        .catch(() => {});
    },
    drop(roomId: string): void {
      generation.set(roomId, (generation.get(roomId) ?? 0) + 1);
      sink.clearRoom(roomId);
    },
  };
}
