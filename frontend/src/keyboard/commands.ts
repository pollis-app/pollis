// Global keyboard commands by stable id; the combo is data, overridable via ./bindings.

export type ShortcutCommandId =
  | "app.toggleSidebar"
  | "app.toggleRightPanel"
  | "app.toggleTerminal"
  | "app.toggleSearch"
  | "app.lock"
  | "app.closeWindow"
  | "app.sync"
  | "nav.back"
  | "search.next"
  | "search.prev"
  | "voice.toggleMute"
  | "voice.toggleDeafen"
  | "voice.pushToTalk"
  | "voice.leave";

export type ShortcutCategory =
  | "Application"
  | "Navigation"
  | "Search"
  | "Voice";

export interface ShortcutCommandMeta {
  id: ShortcutCommandId;
  /** `settings` translation key; translated at render, not import time. */
  titleKey: string;
  category: ShortcutCategory;
  /** Canonical combo: lowercase "+"-joined, mod/meta/ctrl/alt/shift then key; `mod` = meta or ctrl. */
  defaultCombo: string;
  /** Fires on keydown and keyup, force-released on blur (push-to-talk). */
  hold?: boolean;
}

export const SHORTCUT_COMMANDS: Record<
  ShortcutCommandId,
  ShortcutCommandMeta
> = {
  "app.toggleSidebar": {
    id: "app.toggleSidebar",
    titleKey: "shortcuts.appToggleSidebar",
    category: "Application",
    defaultCombo: "mod+b",
  },
  "app.toggleRightPanel": {
    id: "app.toggleRightPanel",
    titleKey: "shortcuts.appToggleRightPanel",
    category: "Application",
    defaultCombo: "mod+shift+b",
  },
  "app.toggleTerminal": {
    id: "app.toggleTerminal",
    titleKey: "shortcuts.appToggleTerminal",
    category: "Application",
    defaultCombo: "mod+`",
  },
  "app.toggleSearch": {
    id: "app.toggleSearch",
    titleKey: "shortcuts.appToggleSearch",
    category: "Application",
    defaultCombo: "mod+k",
  },
  "app.lock": {
    id: "app.lock",
    titleKey: "shortcuts.appLock",
    category: "Application",
    defaultCombo: "mod+l",
  },
  "app.closeWindow": {
    id: "app.closeWindow",
    titleKey: "shortcuts.appCloseWindow",
    category: "Application",
    defaultCombo: "mod+w",
  },
  "app.sync": {
    id: "app.sync",
    titleKey: "shortcuts.appSync",
    category: "Application",
    defaultCombo: "mod+r",
  },
  "nav.back": {
    id: "nav.back",
    titleKey: "shortcuts.navBack",
    category: "Navigation",
    defaultCombo: "escape",
  },
  // Registered above app.toggleSearch, so `mod+k` inside the open menu moves the selection.
  "search.next": {
    id: "search.next",
    titleKey: "shortcuts.searchNext",
    category: "Search",
    defaultCombo: "mod+j",
  },
  "search.prev": {
    id: "search.prev",
    titleKey: "shortcuts.searchPrev",
    category: "Search",
    defaultCombo: "mod+k",
  },
  "voice.toggleMute": {
    id: "voice.toggleMute",
    titleKey: "shortcuts.voiceToggleMute",
    category: "Voice",
    defaultCombo: "mod+shift+m",
  },
  "voice.toggleDeafen": {
    id: "voice.toggleDeafen",
    titleKey: "shortcuts.voiceToggleDeafen",
    category: "Voice",
    defaultCombo: "mod+shift+d",
  },
  "voice.pushToTalk": {
    id: "voice.pushToTalk",
    titleKey: "shortcuts.voicePushToTalk",
    category: "Voice",
    // A modifier combo: there is no "typing" guard, so a bare key would transmit while writing.
    defaultCombo: "mod+shift+space",
    hold: true,
  },
  "voice.leave": {
    id: "voice.leave",
    titleKey: "shortcuts.voiceLeave",
    category: "Voice",
    defaultCombo: "mod+shift+h",
  },
};

export const ALL_SHORTCUT_COMMAND_IDS = Object.keys(
  SHORTCUT_COMMANDS,
) as ShortcutCommandId[];
