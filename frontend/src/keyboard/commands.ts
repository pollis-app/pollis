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
  | "app.toggleTerminal"
  | "app.toggleSearch"
  | "app.lock"
  | "app.closeWindow"
  | "app.sync"
  | "nav.back"
  | "voice.toggleMute"
  | "voice.leave";

export type ShortcutCategory = "Application" | "Navigation" | "Voice";

export interface ShortcutCommandMeta {
  id: ShortcutCommandId;
  /** Human-facing name for a future shortcuts/settings page. */
  title: string;
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
}

export const SHORTCUT_COMMANDS: Record<
  ShortcutCommandId,
  ShortcutCommandMeta
> = {
  "app.toggleSidebar": {
    id: "app.toggleSidebar",
    title: "Toggle sidebar",
    category: "Application",
    defaultCombo: "mod+b",
  },
  "app.toggleTerminal": {
    id: "app.toggleTerminal",
    title: "Toggle terminal",
    category: "Application",
    defaultCombo: "mod+`",
  },
  "app.toggleSearch": {
    id: "app.toggleSearch",
    title: "Open search",
    category: "Application",
    defaultCombo: "mod+k",
  },
  "app.lock": {
    id: "app.lock",
    title: "Lock app",
    category: "Application",
    defaultCombo: "mod+l",
  },
  "app.closeWindow": {
    id: "app.closeWindow",
    title: "Hide / close window",
    category: "Application",
    defaultCombo: "mod+w",
  },
  "app.sync": {
    id: "app.sync",
    title: "Sync (refetch + MLS)",
    category: "Application",
    defaultCombo: "mod+r",
  },
  "nav.back": {
    id: "nav.back",
    title: "Go back",
    category: "Navigation",
    defaultCombo: "escape",
  },
  "voice.toggleMute": {
    id: "voice.toggleMute",
    title: "Toggle mute",
    category: "Voice",
    defaultCombo: "mod+shift+m",
  },
  "voice.leave": {
    id: "voice.leave",
    title: "Leave call",
    category: "Voice",
    defaultCombo: "mod+shift+h",
  },
};

export const ALL_SHORTCUT_COMMAND_IDS = Object.keys(
  SHORTCUT_COMMANDS,
) as ShortcutCommandId[];
