/** `:shortcode:` parsing, resolution and ranking. MIRROR of desktop's emojiShortcodeQuery.ts + emojiShortcodeIndex.ts. */

import { STANDARD_EMOJI } from "../components/emoji/emojiData";
import { emojiDisplayChar } from "../components/emoji/emojiSearch";
import { emojiTokenText } from "../components/emoji/emojiTokens";
import { readSkinTone } from "./emojiPrefs";
import type { CustomEmoji } from "../hooks/queries/useEmoji";

/** Wider than a custom shortcode on purpose: gemoji has `:+1:`, `:-1:`, `:e-mail:`. */
const SHORTCODE_BODY = /[A-Za-z0-9_+-]/;

export const SHORTCODE_MIN_QUERY = 2;

export const SHORTCODE_SUGGESTION_LIMIT = 8;

export interface ShortcodeEntry {
  shortcode: string;
  insertText: string;
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

function opensAt(text: string, index: number): boolean {
  return index === 0 || /\s/.test(text[index - 1]);
}

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

/** The `:name:` ending at `caret`; no minimum length (`:v:` is real). */
export function completedShortcodeAt(
  text: string,
  caret: number,
): CompletedShortcode | null {
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

/** Custom emoji win outright over standard ones. */
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

/** Exact, then prefix, then substring; custom ahead of standard at equal tier. */
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

/** `trailingSpace` matches Slack: a tapped suggestion finishes a word, a typed colon does not. */
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

/** One row per (alias, emoji) pair, against the stored skin tone. */
export function standardShortcodeEntries(): ShortcodeEntry[] {
  const toneIndex = readSkinTone();
  const entries: ShortcodeEntry[] = [];
  for (const emoji of STANDARD_EMOJI) {
    if (emoji.shortcodes.length === 0) {
      continue;
    }
    const char = emojiDisplayChar(emoji, toneIndex);
    for (const shortcode of emoji.shortcodes) {
      entries.push({
        shortcode,
        insertText: char,
        custom: false,
        char,
        contentHash: null,
        label: emoji.name,
      });
    }
  }
  return entries;
}

export function customShortcodeEntries(
  emoji: readonly CustomEmoji[],
): ShortcodeEntry[] {
  return emoji.map((e) => ({
    shortcode: e.shortcode,
    insertText: emojiTokenText(e.shortcode, e.content_hash),
    custom: true,
    char: null,
    contentHash: e.content_hash,
    label: e.group_name,
  }));
}
