// Global keyboard commands: stable ids with the default combo as data.
// Remapping writes an override for an id (./bindings), never edits a component.

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
  /** `settings` translation key, resolved at each render site (not at import). */
  titleKey: string;
  category: ShortcutCategory;
  /**
   * Canonical lowercase tokens joined by "+", ordered mod, meta, ctrl, alt,
   * shift, key. `mod` matches metaKey or ctrlKey.
   */
  defaultCombo: string;
  /**
   * Hold-style: fires on key down and up, force-released on focus loss
   * (push-to-talk). Default false.
   */
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
    // The sidebar's mod+b, shifted, for the opposite edge.
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
  // Only while the search menu is open, registered above app.toggleSearch so
  // the `mod+k` that opened it moves the selection. j/k follow vim.
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
    // A modifier combo on purpose: with no "is typing" guard, a bare key would
    // transmit while writing a message mid-call.
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
