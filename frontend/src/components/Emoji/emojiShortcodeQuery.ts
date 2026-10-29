/**
 * `:shortcode:` parsing and ranking for the composer. No `emojiData.ts` import
 * (it is code-split, #874): callers pass a flat `ShortcodeEntry` list.
 *
 * Two grammars: `shortcodeQueryAt` (an unterminated `:jo` under the caret,
 * driving suggestions) and `completedShortcodeAt` (a closed `:joy:`,
 * substituted on the second colon, Slack-style). Both open only at string start
 * or after whitespace, so `http://` and `10:30` never match.
 */

import type { StandardEmoji } from "./emojiData";

/**
 * Characters allowed after the opening ':'. Wider than custom shortcodes
 * (`[a-z0-9_]{2,32}`, `SHORTCODE_RE`) because gemoji has `:+1:` / `:e-mail:`.
 */
const SHORTCODE_BODY = /[A-Za-z0-9_+-]/;

/** Shortest query that opens the suggestion list. Discord's rule. */
export const SHORTCODE_MIN_QUERY = 2;

/** Max rows offered at once, matching the mention list's depth. */
export const SHORTCODE_SUGGESTION_LIMIT = 8;

/** One thing a `:shortcode:` can resolve to, custom or standard. */
export interface ShortcodeEntry {
  shortcode: string;
  insertText: string;
  /** A group's custom emoji; custom wins every tie. */
  custom: boolean;
  char: string | null;
  contentHash: string | null;
  label: string;
}

/** An in-progress `:…` the caret is sitting inside. */
export interface ShortcodeQuery {
  start: number;
  end: number;
  query: string;
}

/** A ':' at `index` may open a shortcode only at string start or after whitespace. */
function opensAt(text: string, index: number): boolean {
  return index === 0 || /\s/.test(text[index - 1]);
}

/**
 * The shortcode the caret is typing, or null. Like `mentionQueryAt`, only an
 * unterminated run right before the caret counts.
 */
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

/** A closed `:shortcode:` ending exactly at the caret. */
export interface CompletedShortcode {
  start: number;
  end: number;
  name: string;
}

/**
 * The `:name:` ending at `caret`, or null. No minimum length (`:v:` is real);
 * `SHORTCODE_MIN_QUERY` only governs when the list opens.
 */
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

/** The entry a completed `:name:` resolves to. Custom wins: a group's own `:tada:` beats Unicode's. */
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

/**
 * Entries matching `query`, best first: exact, prefix, substring; custom before
 * standard within a tier (as `searchEmoji`), then shorter, then alphabetical.
 * Nothing below `SHORTCODE_MIN_QUERY` characters.
 */
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

/**
 * Replace `[start, end)` with `insertText`, returning the text and caret.
 * `trailingSpace` follows Slack: picking from the list adds a space; typing the
 * closing colon doesn't.
 */
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
