import { create } from "zustand";

/// Per-peer "their identity key just changed" signal. Pushed by the
/// `key_changed` realtime event the backend emits whenever
/// `check_and_pin_account_key` observes a mismatch against the TOFU pin.
///
/// Policy is ADVISORY-with-acknowledge (matches the existing
/// `check_and_pin_account_key` comment: "advisory — the caller does not
/// block delivery"). Sends still work; the banner just nudges the user
/// to re-verify out-of-band. Acknowledging the banner only dismisses it
/// from the UI — the verified flag in the local DB stays cleared until
/// the user explicitly re-verifies on the profile page.
interface KeyChangeStore {
  // peerUserId → version observed at the time of the change. We key by
  // peerUserId (not conversation_id) because the same peer may be in
  // several DMs and the warning is per-identity, not per-conversation.
  flagged: Record<string, { peerIdentityVersion: number; observedAt: number }>;
  flagChanged: (peerUserId: string, peerIdentityVersion: number) => void;
  acknowledge: (peerUserId: string) => void;
  clearAll: () => void;
}

export const useKeyChangeStore = create<KeyChangeStore>((set) => ({
  flagged: {},
  flagChanged: (peerUserId, peerIdentityVersion) => {
    set((state) => ({
      flagged: {
        ...state.flagged,
        [peerUserId]: {
          peerIdentityVersion,
          observedAt: Date.now(),
        },
      },
    }));
  },
  acknowledge: (peerUserId) => {
    set((state) => {
      if (!(peerUserId in state.flagged)) {
        return state;
      }
      const next = { ...state.flagged };
      delete next[peerUserId];
      return { flagged: next };
    });
  },
  clearAll: () => set({ flagged: {} }),
}));
