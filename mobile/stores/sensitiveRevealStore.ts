// Revealed sensitive attachments (#1568), per attachment id, in memory only (desktop's twin).

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
