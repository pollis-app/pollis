// A command id's display label, through the same `resolveCombo` the keydown
// listener uses, re-rendering when an override changes.

import { useSyncExternalStore } from "react";
import { useTranslation } from "react-i18next";

import { resolveCombo, subscribeShortcutOverrides } from "./bindings";
import type { ShortcutCommandId } from "./commands";
import { formatCombo } from "./keyCombo";

export function useShortcutLabel(id: ShortcutCommandId): string {
  // `formatCombo` translates key words, so re-render on language change too.
  // Defensive (current consumers translate anyway); the value isn't read.
  useTranslation();
  return useSyncExternalStore(
    subscribeShortcutOverrides,
    () => formatCombo(resolveCombo(id)),
    () => formatCombo(resolveCombo(id)),
  );
}
