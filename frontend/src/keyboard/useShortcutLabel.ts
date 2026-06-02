// Render-side helper that turns a stable command id into a display label
// honoring the user's override map. Calls `resolveCombo(id)` (the same
// resolver the keydown listener uses) and pipes the result through
// `formatCombo`, so a UI hint like the Cmd+K badge in the breadcrumb or
// the Cmd+B badge on the sidebar collapse handle automatically picks up
// any override the user has set in Preferences — and falls back to the
// `defaultCombo` from `commands.ts` when no override exists.
//
// Subscribes to `subscribeShortcutOverrides` so the badge re-renders
// without a page reload when the user edits a binding.

import { useSyncExternalStore } from "react";

import { resolveCombo, subscribeShortcutOverrides } from "./bindings";
import type { ShortcutCommandId } from "./commands";
import { formatCombo } from "./keyCombo";

export function useShortcutLabel(id: ShortcutCommandId): string {
  return useSyncExternalStore(
    subscribeShortcutOverrides,
    () => formatCombo(resolveCombo(id)),
    () => formatCombo(resolveCombo(id)),
  );
}
