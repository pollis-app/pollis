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
import { useTranslation } from "react-i18next";

import { resolveCombo, subscribeShortcutOverrides } from "./bindings";
import type { ShortcutCommandId } from "./commands";
import { formatCombo } from "./keyCombo";

export function useShortcutLabel(id: ShortcutCommandId): string {
  // `formatCombo` now translates its modifier and key WORDS (Ctrl → Strg), so
  // this label has to re-render on a language change and not only on a rebind.
  // `useSyncExternalStore` wakes for its own store only; it re-reads the
  // snapshot on any re-render, but nothing here makes one happen.
  //
  // DEFENSIVE, and honestly so: all six current consumers (BreadcrumbNav,
  // SecurityPage, PreferencesPage, VoiceBar, SidebarVoiceControls,
  // VoiceInputModeSelect) already call `useTranslation` themselves, so they
  // re-render anyway and no test can currently distinguish this line from its
  // absence. It is here so the hook is correct ON ITS OWN — the seventh
  // consumer, which renders nothing else translatable, would otherwise sit on
  // a stale label with nothing on screen to explain it. The value is not read;
  // the subscription is the whole point.
  useTranslation();
  return useSyncExternalStore(
    subscribeShortcutOverrides,
    () => formatCombo(resolveCombo(id)),
    () => formatCombo(resolveCombo(id)),
  );
}
