import { makeAutoObservable } from "mobx";

/**
 * Whether the right-hand context panel is open (#904).
 *
 * WHY THIS IS NOT IN THE URL
 * --------------------------
 * It used to be, as `?panel=`. Nothing in the app carries search params across
 * a navigation — every `router.navigate({ to })` in the sidebar produces a
 * bare URL — so the param was dropped on every click and the panel fell back
 * to a default derived from the SKIN. In `terminal` that default was "shut",
 * which is why the panel slammed closed every time the user moved; in
 * `refined` it was "open", which is why closing it never stuck. The panel's
 * open state is not a property of a location, so it does not live in one.
 *
 * WHY THIS IS NOT IN THE SYNCED PREFERENCES BLOB EITHER
 * ----------------------------------------------------
 * Whether a column of members is worth the pixels is a question about THIS
 * screen: a 13" laptop and a desktop ultrawide want different answers, and
 * syncing would force one on both. Same reasoning as the device-local language
 * (`i18n/storage.ts`) and font size (`loadDeviceFontSize`), and the same
 * mechanism — a `localStorage` entry KEYED BY USER ID, so a shared OS account
 * keeps each Pollis user's answer separate and no one inherits a colleague's.
 *
 * The synced `right_panel_open_by_default` preference still exists, but only
 * as the SEED for a device that has no answer yet — see `useRightPanel`.
 */

const OPEN_KEY_PREFIX = "pollis-right-panel-open:";

/** Matches `fontSizeKey`: signed-out falls back to a single `anon` scope. */
function openKey(userId: string | null | undefined): string {
  return `${OPEN_KEY_PREFIX}${userId ?? "anon"}`;
}

/**
 * This device's remembered answer for `userId`, or null when it has none yet.
 *
 * Null is a distinct third value, not "closed": it is what tells the caller to
 * fall back to the seed instead of showing the user a panel they never shut.
 */
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
    // localStorage unavailable (private mode, disabled storage) — treat as unset.
    return null;
  }
}

/** Persists this device's answer for `userId`. */
export function saveDeviceRightPanelOpen(
  userId: string | null | undefined,
  open: boolean,
): void {
  try {
    localStorage.setItem(openKey(userId), open ? "true" : "false");
  } catch {
    // localStorage unavailable / quota exceeded — fall through silently.
  }
}

class RightPanelStore {
  /**
   * Scope key → remembered state, for scopes touched in this session.
   *
   * The observable half of the pair: `localStorage` cannot be reacted to, so a
   * write has to land somewhere MobX can see or the panel would not repaint
   * until the next render for an unrelated reason. Keyed by the same string
   * the storage entry uses, so the two halves cannot drift apart.
   */
  private byScope: Record<string, boolean> = {};

  constructor() {
    // `remembered` is excluded from auto-annotation on purpose. Actions run
    // untracked, so as an action its `byScope` read would not be tracked by
    // the observing component and toggling the panel would not repaint it.
    // Same reason `presenceStore.isOnline` is excluded.
    makeAutoObservable(this, { remembered: false }, { autoBind: true });
  }

  /**
   * What this device remembers for `userId`, or null if it remembers nothing.
   *
   * Reads the in-session map first and only then falls back to storage, so a
   * value written this session is served from the observable copy and the
   * disk read happens once per scope.
   */
  remembered(userId: string | null): boolean | null {
    const key = openKey(userId);
    if (key in this.byScope) {
      return this.byScope[key];
    }
    return loadDeviceRightPanelOpen(userId);
  }

  /** Records `open` for `userId`, in memory and on this device. */
  setOpen(userId: string | null, open: boolean) {
    this.byScope = { ...this.byScope, [openKey(userId)]: open };
    saveDeviceRightPanelOpen(userId, open);
  }
}

export const rightPanelStore = new RightPanelStore();
