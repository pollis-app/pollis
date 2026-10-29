import { makeAutoObservable } from "mobx";

// Which sensitive attachments (#1568) the user has revealed, by attachment id.
//
// In memory only, so a restart hides everything again — the point of marking
// media sensitive is that it is not on screen when someone looks over your
// shoulder, and a reveal that outlived the session would defeat that. One
// store rather than component state so every surface that draws the same
// attachment — the message, the right panel's media grid, the gallery and its
// viewer — agrees: reveal it in the message and the gallery does not re-blur
// it. Keyed per attachment, never per message: revealing one picture in a
// message leaves its neighbours hidden.
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
