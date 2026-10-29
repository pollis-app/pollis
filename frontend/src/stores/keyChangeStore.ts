import { makeAutoObservable } from "mobx";

/// Per-peer identity-key-change flags from `key_changed`. Advisory: sends work; dismissing doesn't restore "verified".
interface KeyChangeFlag {
  peerIdentityVersion: number;
  observedAt: number;
}

class KeyChangeStore {
  // Keyed by peer, since the warning is per identity.
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
