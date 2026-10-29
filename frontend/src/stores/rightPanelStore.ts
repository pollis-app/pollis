import { makeAutoObservable } from "mobx";

/**
 * Whether the right panel is open (#904): device-local `localStorage` keyed by
 * user id, neither in the URL (params don't survive navigation) nor synced
 * (it's about THIS screen). The synced `right_panel_open_by_default` only
 * seeds a device with no answer. See `.codesight/wiki/ui.md` → "The right panel".
 */

const OPEN_KEY_PREFIX = "pollis-right-panel-open:";

function openKey(userId: string | null | undefined): string {
  return `${OPEN_KEY_PREFIX}${userId ?? "anon"}`;
}

/** This device's answer for `userId`, or null (fall back to the seed, not "closed"). */
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
      // localStorage unavailable: treat as unset.
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
    // localStorage unavailable / quota exceeded: ignore.
  }
}

class RightPanelStore {
  /**
   * Scope key → state for scopes touched this session: the observable copy,
   * since `localStorage` can't be reacted to. Same key as the storage entry.
   */
  private byScope: Record<string, boolean> = {};

  constructor() {
    // `remembered` stays a plain method so its reads are tracked (see presenceStore).
    makeAutoObservable(this, { remembered: false }, { autoBind: true });
  }

  /** The in-session value, else storage (read once per scope), else null. */
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
