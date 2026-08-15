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
  // Space arrives as " ", which cannot survive a "+"-joined combo string:
  // parseCombo trims every token, so a literal space would be dropped and
  // the binding would silently degrade to modifiers-only. Spell it out.
  if (key === " ") {
    return "space";
  }
  // Single printable chars ("b", "`", "/") -> lowercased. Named keys
  // ("Escape", "ArrowUp") -> lowercased name. Both collapse via toLowerCase.
  return key.toLowerCase();
}

/** Modifier `KeyboardEvent.key` values, normalized. */
const MODIFIER_KEYS = new Set(["control", "meta", "alt", "shift"]);

export function isModifierKey(key: string): boolean {
  return MODIFIER_KEYS.has(normalizeKey(key));
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

/**
 * Does this keyup end a currently-held combo?
 *
 * Looser than `comboMatchesEvent` on purpose. Releasing `mod+shift+space`
 * rarely lifts all three keys at once — let go of Shift first and the
 * *next* keyup carries `shiftKey: false`, so an exact-modifier match would
 * never fire and the key would appear stuck down forever. Any constituent
 * of the combo going up ends the hold, which for push-to-talk is the safe
 * direction to be wrong in.
 */
export function comboReleasedByEvent(p: ParsedCombo, e: KeyboardEvent): boolean {
  const k = normalizeKey(e.key);
  if (k === p.key) {
    return true;
  }
  if (k === "control" && (p.ctrl || p.mod)) {
    return true;
  }
  if (k === "meta" && (p.meta || p.mod)) {
    return true;
  }
  if (k === "alt" && p.alt) {
    return true;
  }
  if (k === "shift" && p.shift) {
    return true;
  }
  return false;
}

const KEY_LABELS: Record<string, string> = {
  escape: "Esc",
  arrowup: "↑",
  arrowdown: "↓",
  arrowleft: "←",
  arrowright: "→",
  space: "Space",
  "`": "`",
};

/**
 * Human-facing label for a combo, e.g. "⌘ K" on macOS / "Ctrl+K" elsewhere.
 * Thin space (U+2009) between modifier glyphs and key on macOS gives the
 * kbd badge some breathing room; other platforms use the conventional "+".
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

  // macOS condenses modifier glyphs against the key; a thin space (U+2009)
  // gives the kbd badge a little breathing room without bloating to a full
  // space. Other platforms use the conventional "+" separator.
  return isMac ? parts.join(" ") : parts.join("+");
}
