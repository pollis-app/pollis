// The single source of truth for global keyboard commands.
//
// A command is identified by a stable, binding-independent id. The key
// combo is *data* attached to the id, not something hardcoded at the call
// site. Today the combo comes from `defaultCombo`; when user-configurable
// shortcuts land, an override layer (see resolveCombo in ./bindings) is
// consulted first and the rest of the system — registry, hook, a future
// settings/help page — keeps working unchanged. Remapping a shortcut will
// mean writing an override for an id, never editing a component.

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
  /**
   * Translation key for the human-facing name, in the `settings` namespace.
   *
   * A key rather than the copy itself: this table is module-level data,
   * evaluated once at import time, so a `t()` call here would snapshot
   * whatever language was active on the very first import and never update.
   * Every consumer translates at its own render site instead.
   */
  titleKey: string;
  /** Grouping for that future page. */
  category: ShortcutCategory;
  /**
   * Default key combo. Canonical form: lowercase tokens joined by "+", in
   * the order mod, meta, ctrl, alt, shift, then the key. `mod` is the
   * platform primary (Cmd on macOS, Ctrl elsewhere) and matches either
   * metaKey or ctrlKey, mirroring the app's prior `e.metaKey || e.ctrlKey`
   * convention. This string is JSON-serializable so a future override map
   * can be persisted in preferences verbatim.
   */
  defaultCombo: string;
  /**
   * Hold-style command: fires on key *down* and again on key *up*, and is
   * force-released whenever the window loses focus. Push-to-talk is the
   * only one today. Default false — an ordinary command only ever fires on
   * keydown.
   *
   * The rebinding UI shows these identically to any other shortcut; the
   * difference is purely in how the registry dispatches them.
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
    // Mirrors mod+b for the left sidebar, shifted — the two panels are the
    // same gesture on opposite edges.
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
  // Only dispatched while the search menu is open, and registered above
  // app.toggleSearch so the `mod+k` that opened the menu moves the
  // selection instead of closing it again — Esc already closes, and a
  // toggle that fires from inside the menu is a wasted key. The j/k
  // direction follows vim, which is where every app that offers this
  // (Linear, GitHub, Slack's quick switcher) took it from.
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
    // Sits next to Toggle mute on the same modifier pair, the way Discord
    // pairs the two controls.
    defaultCombo: "mod+shift+d",
  },
  "voice.pushToTalk": {
    id: "voice.pushToTalk",
    titleKey: "shortcuts.voicePushToTalk",
    category: "Voice",
    // Deliberately a modifier combo rather than a bare key. The dispatcher
    // has no "is the user typing" guard, so a bare-key default would
    // transmit every time you wrote a message mid-call. Real PTT users
    // rebind this to whatever is under their thumb — that is what the
    // rebinding UI is for — but the shipped default must be safe.
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
