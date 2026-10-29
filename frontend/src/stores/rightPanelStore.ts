import { makeAutoObservable } from "mobx";

/** Right-panel open state (#904): device-local per user, not URL (dropped on navigation) or synced (screen-specific). */

const OPEN_KEY_PREFIX = "pollis-right-panel-open:";

/** Matches `fontSizeKey`: signed-out falls back to a single `anon` scope. */
function openKey(userId: string | null | undefined): string {
  return `${OPEN_KEY_PREFIX}${userId ?? "anon"}`;
}

/** This device's answer for `userId`, or null (fall back to the seed). */
export function loadDeviceRightPanelOpen(
  userId: string | null | undefined,
): boolean | null {
  try {
    const raw = localStorage.getItem(openKey(userId));
    if (raw === "true") {
      return true;
    }
    if (raw === "false") {
      return false;
    }
    return null;
  } catch {
    return null;
  }
}

export function saveDeviceRightPanelOpen(
  userId: string | null | undefined,
  open: boolean,
): void {
  try {
    localStorage.setItem(openKey(userId), open ? "true" : "false");
  } catch {
  }
}

class RightPanelStore {
  /** Scope key → state; the observable twin of `localStorage`. */
  private byScope: Record<string, boolean> = {};

  constructor() {
    // `remembered` stays tracked (not an action), as with `presenceStore.isOnline`.
    makeAutoObservable(this, { remembered: false }, { autoBind: true });
  }

  /** In-session value first, else storage (read once per scope). */
  remembered(userId: string | null): boolean | null {
    const key = openKey(userId);
    if (key in this.byScope) {
      return this.byScope[key];
    }
    return loadDeviceRightPanelOpen(userId);
  }

  setOpen(userId: string | null, open: boolean) {
    this.byScope = { ...this.byScope, [openKey(userId)]: open };
    saveDeviceRightPanelOpen(userId, open);
  }
}

export const rightPanelStore = new RightPanelStore();
