// Presence (#2059, #2110), desktop's rule: a user is online while any room this device shares with
// them shows them in use, away while every room that shows them shows them not in use, and offline
// while none shows them. Pure, so the store and the realtime feeder are tested without a device.

export type PresenceStatus = "online" | "away" | "offline";

/** One room's answer from the core: the users in use there, and those whose every session is not. */
export type RoomSnapshot = { readonly online: readonly string[]; readonly away: readonly string[] };

/** room id → what that room shows (never this user: the core leaves self out). */
export type RoomPresence = Readonly<Record<string, RoomSnapshot>>;

const EMPTY: RoomSnapshot = { online: [], away: [] };

const strings = (v: unknown): string[] =>
  Array.isArray(v) ? v.filter((x): x is string => typeof x === "string") : [];

/**
 * The core's answer as a snapshot. A bare list (a core that predates #2110) is everyone online;
 * anything unreadable is no one.
 */
export function snapshotOf(answer: unknown): RoomSnapshot {
  if (Array.isArray(answer)) {
    return { online: strings(answer), away: [] };
  }
  if (answer && typeof answer === "object") {
    const a = answer as { online?: unknown; away?: unknown };
    return { online: strings(a.online), away: strings(a.away) };
  }
  return EMPTY;
}

/** Replace one room's snapshot; an empty one drops the room. A user listed both ways is online. */
export function withRoom(state: RoomPresence, roomId: string, snapshot: RoomSnapshot): RoomPresence {
  const online = [...new Set(snapshot.online)];
  const away = [...new Set(snapshot.away)].filter((u) => !online.includes(u));
  const next = { ...state };
  if (online.length === 0 && away.length === 0) {
    delete next[roomId];
  } else {
    next[roomId] = { online, away };
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

/** Everyone some room shows → whether some room shows them in use. */
export function presentUsers(state: RoomPresence): Map<string, boolean> {
  const out = new Map<string, boolean>();
  for (const room of Object.values(state)) {
    for (const userId of room.away) {
      if (!out.has(userId)) {
        out.set(userId, false);
      }
    }
    for (const userId of room.online) {
      out.set(userId, true);
    }
  }
  return out;
}

/** Self is always online on its own device; the rooms never report it. */
export function statusOf(
  present: ReadonlyMap<string, boolean>,
  userId: string | null | undefined,
  selfUserId: string | null | undefined,
): PresenceStatus {
  if (!userId) {
    return "offline";
  }
  if (userId === selfUserId) {
    return "online";
  }
  const inUse = present.get(userId);
  if (inUse === undefined) {
    return "offline";
  }
  return inUse ? "online" : "away";
}

// ── this device's side (#2110) ───────────────────────────────────────────────

/** The LiveKit participant attribute every client sets: "1" in use, "0" running but not in use. */
export const ACTIVE_ATTRIBUTE = "active";

/** What the attribute says for `inUse`. */
export function activeAttributes(inUse: boolean): Record<string, string> {
  return { [ACTIVE_ATTRIBUTE]: inUse ? "1" : "0" };
}

/**
 * Whether a session counts as in use, from its attributes. Only an explicit "0" is not: a build
 * that predates the attribute sets none and must still show online.
 */
export function sessionInUse(attributes: Readonly<Record<string, string>> | undefined): boolean {
  return attributes?.[ACTIVE_ATTRIBUTE] !== "0";
}

/**
 * Whether the app is in use, from React Native's `AppState`: only the background is not. iOS's
 * `inactive` (the app switcher, a call banner, Control Centre) passes in a moment.
 */
export function appStateInUse(state: string | null | undefined): boolean {
  return state !== "background";
}

/**
 * The corner dot's geometry for an avatar `size` points across, desktop's (`ui/Avatar.tsx`): a
 * quarter of the avatar (5 at least), a 2-point ring, sitting 2 points past the corner. Away bites
 * a circle four fifths of the dot out of its top-left corner, in the ring's colour: a crescent in
 * the same box.
 */
export function presenceDotGeometry(size: number): { dot: number; ring: number; offset: number } {
  return { dot: Math.max(5, Math.round(size * 0.24)), ring: 2, offset: -2 };
}

/** The crescent's bite for a dot `dot` points across: its size and how far past the top-left. */
export function awayBite(dot: number): { size: number; inset: number } {
  return { size: Math.round(dot * 0.8), inset: -Math.round(dot * 0.2) };
}

type Resolve = (roomId: string, identities: string[], inactive: string[]) => Promise<unknown>;

export interface PresenceSink {
  setRoom(roomId: string, snapshot: RoomSnapshot): void;
  clearRoom(roomId: string): void;
}

/**
 * Feeds a sink from LiveKit rooms: each refresh hands the core every remote participant's wire
 * identity, and the ones whose `active` attribute says not in use, and stores the answer. Only the
 * newest refresh of a room lands, and none lands after the room was dropped. A failed resolve
 * (offline, or a binary without the command) leaves the room as it was.
 */
export function createRoomPresenceFeed(resolve: Resolve, sink: PresenceSink) {
  const generation = new Map<string, number>();

  return {
    refresh(roomId: string, identities: string[], inactive: string[] = []): Promise<void> {
      const gen = (generation.get(roomId) ?? 0) + 1;
      generation.set(roomId, gen);
      return resolve(roomId, identities, inactive)
        .then((answer) => {
          if (generation.get(roomId) !== gen) {
            return;
          }
          sink.setRoom(roomId, snapshotOf(answer));
        })
        .catch(() => {});
    },
    drop(roomId: string): void {
      generation.set(roomId, (generation.get(roomId) ?? 0) + 1);
      sink.clearRoom(roomId);
    },
  };
}
