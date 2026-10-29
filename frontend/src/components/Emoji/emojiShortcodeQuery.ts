/** `:shortcode:` parsing and ranking (#848). No `emojiData.ts` import (code-split, #874). Shortcodes open only at string start or after whitespace. */

import type { StandardEmoji } from "./emojiData";

/** Wider than custom shortcodes (gemoji has `+`/`-`); custom stays `[a-z0-9_]{2,32}`. */
const SHORTCODE_BODY = /[A-Za-z0-9_+-]/;

/** Shortest query that opens the list (Discord's rule). */
export const SHORTCODE_MIN_QUERY = 2;

export const SHORTCODE_SUGGESTION_LIMIT = 8;

export interface ShortcodeEntry {
  shortcode: string;
  insertText: string;
  /** A group's custom emoji; wins every tie. */
  custom: boolean;
  char: string | null;
  contentHash: string | null;
  label: string;
}

export interface ShortcodeQuery {
  start: number;
  end: number;
  query: string;
}

/** A ':' opens a shortcode only at string start or after whitespace. */
function opensAt(text: string, index: number): boolean {
  return index === 0 || /\s/.test(text[index - 1]);
}

/** The unterminated shortcode right before the caret, or null. */
export function shortcodeQueryAt(text: string, caret: number): ShortcodeQuery | null {
  let i = caret;
  while (i > 0 && SHORTCODE_BODY.test(text[i - 1])) {
    i -= 1;
  }
  if (i === 0 || text[i - 1] !== ":") {
    return null;
  }
  const colon = i - 1;
  if (!opensAt(text, colon)) {
    return null;
  }
  return { start: colon, end: caret, query: text.slice(i, caret).toLowerCase() };
}

export interface CompletedShortcode {
  start: number;
  end: number;
  name: string;
}

/** The closed `:name:` ending at `caret`; no minimum length (`:v:` exists). */
export function completedShortcodeAt(text: string, caret: number): CompletedShortcode | null {
  if (caret <= 0 || text[caret - 1] !== ":") {
    return null;
  }
  let i = caret - 1;
  while (i > 0 && SHORTCODE_BODY.test(text[i - 1])) {
    i -= 1;
  }
  if (i === caret - 1) {
    return null;
  }
  if (i === 0 || text[i - 1] !== ":") {
    return null;
  }
  const colon = i - 1;
  if (!opensAt(text, colon)) {
    return null;
  }
  return { start: colon, end: caret, name: text.slice(i, caret - 1).toLowerCase() };
}

/** Custom emoji win outright. */
export function resolveShortcode(
  entries: readonly ShortcodeEntry[],
  name: string,
): ShortcodeEntry | undefined {
  let standard: ShortcodeEntry | undefined;
  for (const entry of entries) {
    if (entry.shortcode !== name) {
      continue;
    }
    if (entry.custom) {
      return entry;
    }
    standard = standard ?? entry;
  }
  return standard;
}

/** Exact, prefix, then substring; custom first per tier, then shorter, then alias. Nothing below `SHORTCODE_MIN_QUERY`. */
export function rankShortcodeEntries(
  entries: readonly ShortcodeEntry[],
  query: string,
  limit: number = SHORTCODE_SUGGESTION_LIMIT,
): ShortcodeEntry[] {
  const needle = query.toLowerCase();
  if (needle.length < SHORTCODE_MIN_QUERY) {
    return [];
  }
  const scored: { entry: ShortcodeEntry; rank: number }[] = [];
  for (const entry of entries) {
    const index = entry.shortcode.indexOf(needle);
    if (index < 0) {
      continue;
    }
    let tier: number;
    if (entry.shortcode === needle) {
      tier = 0;
    } else if (index === 0) {
      tier = 1;
    } else {
      tier = 2;
    }
    scored.push({ entry, rank: tier * 2 + (entry.custom ? 0 : 1) });
  }
  scored.sort(
    (a, b) =>
      a.rank - b.rank ||
      a.entry.shortcode.length - b.entry.shortcode.length ||
      a.entry.shortcode.localeCompare(b.entry.shortcode),
  );
  return scored.slice(0, limit).map((s) => s.entry);
}

/** Splice `insertText` in; `trailingSpace` for list picks, not for a typed closing colon. */
export function applyShortcode(
  text: string,
  start: number,
  end: number,
  insertText: string,
  trailingSpace: boolean,
): { text: string; caret: number } {
  const inserted = trailingSpace ? `${insertText} ` : insertText;
  return {
    text: text.slice(0, start) + inserted + text.slice(end),
    caret: start + inserted.length,
  };
}

export function standardEntry(
  emoji: StandardEmoji,
  shortcode: string,
  displayChar: string,
): ShortcodeEntry {
  return {
    shortcode,
    insertText: displayChar,
    custom: false,
    char: displayChar,
    contentHash: null,
    label: emoji.name,
  };
}
