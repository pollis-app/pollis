// The configurability seam.
//
// `resolveCombo` is the *only* place that decides which combo a command is
// bound to. Today it returns the default. When user-configurable shortcuts
// land, a persisted override map (loaded from preferences) layers on top
// here — and nothing else in the system has to change: the registry calls
// resolveCombo on every keydown, so new overrides take effect immediately.
//
// No remapping UI / persistence is wired yet by design; this just defines
// the shape so the rest of the module is already future-proof.

import {
  SHORTCUT_COMMANDS,
  type ShortcutCommandId,
} from "./commands";

type OverrideMap = Partial<Record<ShortcutCommandId, string>>;

let overrides: OverrideMap = {};
const subscribers = new Set<() => void>();

/**
 * Resolve the active combo for a command: a user override if one exists,
 * otherwise the built-in default. Cheap enough to call per keydown.
 */
export function resolveCombo(id: ShortcutCommandId): string {
  return overrides[id] ?? SHORTCUT_COMMANDS[id].defaultCombo;
}

/**
 * Replace the override map (future: called once user prefs load, and on
 * every edit in a settings page). Notifies subscribers so anything caching
 * resolved combos — e.g. a shortcuts/help list — can refresh. The registry
 * resolves lazily per keydown and needs no subscription.
 */
export function setShortcutOverrides(next: OverrideMap): void {
  overrides = { ...next };
  for (const fn of subscribers) {
    fn();
  }
}

export function getShortcutOverrides(): Readonly<OverrideMap> {
  return overrides;
}

export function subscribeShortcutOverrides(fn: () => void): () => void {
  subscribers.add(fn);
  return () => {
    subscribers.delete(fn);
  };
}
