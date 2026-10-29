// Combo parse/match/format; the persisted combo format stays here.

import i18n from "../i18n";
import { isMac } from "../utils/platform";

export interface ParsedCombo {
  /** Platform primary: matches metaKey OR ctrlKey (the app's convention). */
  mod: boolean;
  /** Specifically ctrlKey. */
  ctrl: boolean;
  /** Specifically metaKey. */
  meta: boolean;
  alt: boolean;
  shift: boolean;
  /** Normalized key, e.g. "b", "escape", "`". */
  key: string;
}

/** Normalize a KeyboardEvent.key to the canonical token used in combos. */
export function normalizeKey(key: string): string {
  // Space can't survive the trimmed "+"-joined format, so spell it out.
  if (key === " ") {
    return "space";
  }
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

/** Exact-modifier match; `mod` is meta or ctrl, and unrequested modifiers must be absent. */
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

/** Whether a keyup ends a held combo: any constituent key going up (modifiers lift one at a time). */
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

/** Key names that are words, so translated at call time. */
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

/** Key glyphs: not copy, never translated or mirrored. */
const KEY_GLYPHS: Record<string, string> = {
  arrowup: "↑",
  arrowdown: "↓",
  arrowleft: "←",
  arrowright: "→",
  "`": "`",
};

/** Label for a combo, e.g. "⌘ K" on macOS / "Ctrl+K" elsewhere. */
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

  return isMac ? parts.join(" ") : parts.join("+");
}
