// `resolveCombo` is the only place that decides a command's combo: the user's
// override map (from preferences) over the default, resolved per keydown so
// overrides apply immediately.

import {
  SHORTCUT_COMMANDS,
  type ShortcutCommandId,
} from "./commands";

type OverrideMap = Partial<Record<ShortcutCommandId, string>>;

let overrides: OverrideMap = {};
const subscribers = new Set<() => void>();

/** The active combo for a command: a user override, else the default. Cheap per keydown. */
export function resolveCombo(id: ShortcutCommandId): string {
  return overrides[id] ?? SHORTCUT_COMMANDS[id].defaultCombo;
}

/**
 * Replace the override map and notify subscribers that cache resolved combos.
 * The registry resolves per keydown and needs no subscription.
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
