// Combo parsing, event matching, and display formatting.
//
// A combo string is the serializable contract between commands.ts, a
// future user-override map, and this matcher. Keeping parse/match/format
// here means the persisted format never leaks into call sites.

import { isMac } from "../utils/platform";

export interface ParsedCombo {
  /** Platform primary: matches metaKey OR ctrlKey (the app's convention). */
  mod: boolean;
  /** Specifically ctrlKey (reserved for future explicit bindings). */
  ctrl: boolean;
  /** Specifically metaKey (reserved for future explicit bindings). */
  meta: boolean;
  alt: boolean;
  shift: boolean;
  /** Normalized key, e.g. "b", "escape", "`". */
  key: string;
}

/** Normalize a KeyboardEvent.key to the canonical token used in combos. */
export function normalizeKey(key: string): string {
  // Single printable chars ("b", "`", "/") -> lowercased. Named keys
  // ("Escape", "ArrowUp") -> lowercased name. Both collapse via toLowerCase.
  return key.toLowerCase();
}

export function parseCombo(combo: string): ParsedCombo {
  const tokens = combo
    .toLowerCase()
    .split("+")
    .map((t) => t.trim())
    .filter((t) => t.length > 0);

  const parsed: ParsedCombo = {
    mod: false,
    ctrl: false,
    meta: false,
    alt: false,
    shift: false,
    key: "",
  };

  for (const t of tokens) {
    switch (t) {
      case "mod":
        parsed.mod = true;
        break;
      case "ctrl":
      case "control":
        parsed.ctrl = true;
        break;
      case "meta":
      case "cmd":
      case "command":
        parsed.meta = true;
        break;
      case "alt":
      case "option":
        parsed.alt = true;
        break;
      case "shift":
        parsed.shift = true;
        break;
      default:
        parsed.key = t;
    }
  }

  return parsed;
}

/**
 * Exact-modifier match. `mod` accepts meta or ctrl (preserving the app's
 * historical "Cmd or Ctrl" behavior); any modifier the combo does not ask
 * for must be absent, so `mod+b` and `mod+shift+b` are distinct bindings —
 * a prerequisite for sane remapping.
 */
export function comboMatchesEvent(p: ParsedCombo, e: KeyboardEvent): boolean {
  if (normalizeKey(e.key) !== p.key) {
    return false;
  }

  const wantsPrimary = p.mod || p.ctrl || p.meta;
  const hasPrimary = e.ctrlKey || e.metaKey;

  if (p.mod && !hasPrimary) {
    return false;
  }
  if (p.ctrl && !e.ctrlKey) {
    return false;
  }
  if (p.meta && !e.metaKey) {
    return false;
  }
  if (!wantsPrimary && hasPrimary) {
    return false;
  }
  if (p.shift !== e.shiftKey) {
    return false;
  }
  if (p.alt !== e.altKey) {
    return false;
  }
  return true;
}

const KEY_LABELS: Record<string, string> = {
  escape: "Esc",
  arrowup: "↑",
  arrowdown: "↓",
  arrowleft: "←",
  arrowright: "→",
  " ": "Space",
  "`": "`",
};

/**
 * Human-facing label for a combo, e.g. "⌘K" on macOS / "Ctrl+K" elsewhere.
 * Matches the style of the existing utils/platform `shortcutLabel`.
 */
export function formatCombo(combo: string): string {
  const p = parseCombo(combo);
  const parts: string[] = [];

  if (p.mod || p.meta) {
    parts.push(isMac ? "⌘" : "Ctrl");
  }
  if (p.ctrl && !p.mod) {
    parts.push("Ctrl");
  }
  if (p.alt) {
    parts.push(isMac ? "⌥" : "Alt");
  }
  if (p.shift) {
    parts.push(isMac ? "⇧" : "Shift");
  }

  const keyLabel =
    KEY_LABELS[p.key] ??
    (p.key.length === 1 ? p.key.toUpperCase() : p.key.replace(/^\w/, (c) => c.toUpperCase()));
  parts.push(keyLabel);

  return isMac ? parts.join("") : parts.join("+");
}
