// Display label for a command id, honoring overrides; re-renders when a binding changes.

import { useSyncExternalStore } from "react";
import { useTranslation } from "react-i18next";

import { resolveCombo, subscribeShortcutOverrides } from "./bindings";
import type { ShortcutCommandId } from "./commands";
import { formatCombo } from "./keyCombo";

export function useShortcutLabel(id: ShortcutCommandId): string {
  // Re-render on language change too (`formatCombo` translates key words); the value is unused.
  useTranslation();
  return useSyncExternalStore(
    subscribeShortcutOverrides,
    () => formatCombo(resolveCombo(id)),
    () => formatCombo(resolveCombo(id)),
  );
}
