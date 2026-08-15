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
  | "voice.toggleMute"
  | "voice.toggleDeafen"
  | "voice.pushToTalk"
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
    title: "Toggle sidebar",
    category: "Application",
    defaultCombo: "mod+b",
  },
  "app.toggleRightPanel": {
    id: "app.toggleRightPanel",
    title: "Toggle details panel",
    category: "Application",
    // Mirrors mod+b for the left sidebar, shifted — the two panels are the
    // same gesture on opposite edges.
    defaultCombo: "mod+shift+b",
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
  "voice.toggleDeafen": {
    id: "voice.toggleDeafen",
    title: "Toggle deafen",
    category: "Voice",
    // Sits next to Toggle mute on the same modifier pair, the way Discord
    // pairs the two controls.
    defaultCombo: "mod+shift+d",
  },
  "voice.pushToTalk": {
    id: "voice.pushToTalk",
    title: "Push to talk (hold)",
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
    title: "Leave call",
    category: "Voice",
    defaultCombo: "mod+shift+h",
  },
};

export const ALL_SHORTCUT_COMMAND_IDS = Object.keys(
  SHORTCUT_COMMANDS,
) as ShortcutCommandId[];
