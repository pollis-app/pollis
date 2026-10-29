// Which sensitive attachments (#1568) the user has revealed, by attachment id
// — the mobile twin of desktop's `frontend/src/stores/sensitiveRevealStore.ts`,
// same shape and same rules.
//
// In memory only, so a restart covers everything again: marking media
// sensitive is about what is on screen when someone looks over your shoulder,
// and a reveal that outlived the session would defeat that. One store so the
// message row, the full-screen viewer and the Vault grid agree. Keyed per
// attachment, never per message: revealing one picture leaves its neighbours
// covered.

import { makeAutoObservable } from "mobx";

class SensitiveRevealStore {
  revealed = new Set<string>();

  constructor() {
    makeAutoObservable(this, {}, { autoBind: true });
  }

  isRevealed(id: string): boolean {
    return this.revealed.has(id);
  }

  reveal(id: string) {
    this.revealed.add(id);
  }

  hide(id: string) {
    this.revealed.delete(id);
  }

  clear() {
    this.revealed.clear();
  }
}

export const sensitiveRevealStore = new SensitiveRevealStore();
