// Combo parsing, event matching, and display formatting.
//
// A combo string is the serializable contract between commands.ts, a
// future user-override map, and this matcher. Keeping parse/match/format
// here means the persisted format never leaks into call sites.

import i18n from "../i18n";
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

/**
 * Key names that are WORDS, and so are copy: a German keyboard is labelled
 * *Strg* and *Umschalt*, not Ctrl and Shift, and "Esc" is "Esc" only because
 * English abbreviated it that way.
 *
 * Resolved through the i18next singleton at call time, the same way
 * `utils/timeAgo.ts` and `utils/format.ts` do — `formatCombo` is a pure
 * module function shared by the shortcuts page and the ~8 badge call sites
 * behind `useShortcutLabel`, and turning it into a hook to translate three
 * words would be the tail wagging the dog.
 */
const KEY_NAME_KEYS: Record<string, string> = {
  escape: "common:keys.escape",
  space: "common:keys.space",
  enter: "common:keys.enter",
  tab: "common:keys.tab",
  backspace: "common:keys.backspace",
  delete: "common:keys.delete",
  home: "common:keys.home",
  end: "common:keys.end",
  pageup: "common:keys.pageUp",
  pagedown: "common:keys.pageDown",
};

/**
 * Key labels that are GLYPHS, and so are NOT copy.
 *
 * `↑ ↓ ← →` and the backtick are Unicode symbols engraved on the physical
 * keyboard; they read identically in Arabic, German and Chinese, and there is
 * nothing for a translator to do to them. The macOS modifier glyphs
 * `⌘ ⌥ ⇧` below are in this same group and are likewise left alone — Apple
 * ships them untranslated in every localization of macOS. Translating a glyph
 * would only give six locales an opportunity to get it wrong.
 *
 * The arrows are deliberately NOT mirrored under RTL either: they name a key
 * you press, not a direction you read.
 */
const KEY_GLYPHS: Record<string, string> = {
  arrowup: "↑",
  arrowdown: "↓",
  arrowleft: "←",
  arrowright: "→",
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
    parts.push(isMac ? "⌘" : i18n.t("common:keys.ctrl"));
  }
  if (p.ctrl && !p.mod) {
    parts.push(i18n.t("common:keys.ctrl"));
  }
  if (p.alt) {
    parts.push(isMac ? "⌥" : i18n.t("common:keys.alt"));
  }
  if (p.shift) {
    parts.push(isMac ? "⇧" : i18n.t("common:keys.shift"));
  }

  const nameKey = KEY_NAME_KEYS[p.key];
  const keyLabel = nameKey
    ? i18n.t(nameKey)
    : (KEY_GLYPHS[p.key] ??
      (p.key.length === 1
        ? p.key.toUpperCase()
        : p.key.replace(/^\w/, (c) => c.toUpperCase())));
  parts.push(keyLabel);

  // macOS condenses modifier glyphs against the key; a thin space (U+2009)
  // gives the kbd badge a little breathing room without bloating to a full
  // space. Other platforms use the conventional "+" separator.
  return isMac ? parts.join(" ") : parts.join("+");
}
