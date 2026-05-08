import { create } from "zustand";

// Presence is inferred from LiveKit room participation: a user is online as
// long as at least one room they share with us shows them as connected. The
// store tracks the per-user → set-of-rooms map so leaving one room doesn't
// flip them offline if they're still in another shared room.

export type PresenceStatus = "online" | "offline";

interface PresenceStore {
  // user_id → Set<room_id>
  byUser: Record<string, Set<string>>;
  setPresent: (userId: string, roomId: string, present: boolean) => void;
  // Drop every record for the named room — used on `realtime_reconnected`
  // before re-emitting the new participant snapshot.
  resetRoom: (roomId: string) => void;
}

export const usePresenceStore = create<PresenceStore>((set) => ({
  byUser: {},
  setPresent: (userId, roomId, present) => {
    set((state) => {
      const existing = state.byUser[userId];
      const nextSet = new Set(existing ?? []);
      if (present) {
        nextSet.add(roomId);
      } else {
        nextSet.delete(roomId);
      }
      const byUser = { ...state.byUser };
      if (nextSet.size === 0) {
        delete byUser[userId];
      } else {
        byUser[userId] = nextSet;
      }
      return { byUser };
    });
  },
  resetRoom: (roomId) => {
    set((state) => {
      let changed = false;
      const byUser: Record<string, Set<string>> = {};
      for (const [userId, rooms] of Object.entries(state.byUser)) {
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
      return changed ? { byUser } : state;
    });
  },
}));

/**
 * Lightweight selector — returns "online" if we currently see this user in
 * any shared room, otherwise "offline". Designed to be called from the
 * components that render avatars (DM list, search panel, profile page).
 */
export function usePresenceStatus(userId: string | null | undefined): PresenceStatus {
  const present = usePresenceStore((s) =>
    userId ? (s.byUser[userId]?.size ?? 0) > 0 : false,
  );
  return present ? "online" : "offline";
}
