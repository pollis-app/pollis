import { makeAutoObservable } from "mobx";

/// Per-peer "identity key changed" signal from the `key_changed` event. Advisory:
/// sends still work; dismissing hides the banner, but the verified flag stays
/// cleared until the user re-verifies on the profile page.
interface KeyChangeFlag {
  peerIdentityVersion: number;
  observedAt: number;
}

class KeyChangeStore {
  // peerUserId → version seen; per identity, not per conversation.
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
