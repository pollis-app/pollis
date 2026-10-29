// The one place that decides a command's combo; the registry resolves it per keydown.

import {
  SHORTCUT_COMMANDS,
  type ShortcutCommandId,
} from "./commands";

type OverrideMap = Partial<Record<ShortcutCommandId, string>>;

let overrides: OverrideMap = {};
const subscribers = new Set<() => void>();

/** The user override, else the default. */
export function resolveCombo(id: ShortcutCommandId): string {
  return overrides[id] ?? SHORTCUT_COMMANDS[id].defaultCombo;
}

/** Replace the override map and notify subscribers. */
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
