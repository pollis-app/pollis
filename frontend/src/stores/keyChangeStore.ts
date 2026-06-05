import { makeAutoObservable } from "mobx";

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
interface KeyChangeFlag {
  peerIdentityVersion: number;
  observedAt: number;
}

class KeyChangeStore {
  // peerUserId → version observed at the time of the change. We key by
  // peerUserId (not conversation_id) because the same peer may be in
  // several DMs and the warning is per-identity, not per-conversation.
  flagged: Record<string, KeyChangeFlag> = {};

  constructor() {
    makeAutoObservable(this, {}, { autoBind: true });
  }

  flagChanged(peerUserId: string, peerIdentityVersion: number) {
    this.flagged = {
      ...this.flagged,
      [peerUserId]: {
        peerIdentityVersion,
        observedAt: Date.now(),
      },
    };
  }

  acknowledge(peerUserId: string) {
    if (!(peerUserId in this.flagged)) {
      return;
    }
    const next = { ...this.flagged };
    delete next[peerUserId];
    this.flagged = next;
  }

  clearAll() {
    this.flagged = {};
  }
}

export const keyChangeStore = new KeyChangeStore();
