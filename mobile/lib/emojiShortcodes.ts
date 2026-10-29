/**
 * `:shortcode:` parsing, resolution and ranking for the mobile composer.
 * MIRROR of desktop's `emojiShortcodeQuery.ts` plus the index half of
 * `emojiShortcodeIndex.ts`; keep them in step by hand.
 *
 * `shortcodeQueryAt` finds an unterminated `:jo` (the suggestion list);
 * `completedShortcodeAt` a closed `:joy:`, substituted on the second colon.
 * Both open only at the start or after whitespace, so `http://x` and `10:30`
 * never read as shortcodes.
 */

import { STANDARD_EMOJI } from "../components/emoji/emojiData";
import { emojiDisplayChar } from "../components/emoji/emojiSearch";
import { emojiTokenText } from "../components/emoji/emojiTokens";
import { readSkinTone } from "./emojiPrefs";
import type { CustomEmoji } from "../hooks/queries/useEmoji";

/**
 * Wider than a custom shortcode on purpose: gemoji has `:+1:`, `:-1:`,
 * `:e-mail:`. Custom shortcodes stay `[a-z0-9_]{2,32}` (enforced by core and DS).
 */
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

/**
 * The `:name:` that ends at `caret`, or null. No minimum length — `:v:` and
 * `:o:` are real aliases; `SHORTCODE_MIN_QUERY` governs the list, not validity.
 */
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
  // An empty body ("::") is not a shortcode.
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

/**
 * The entry a completed `:name:` resolves to. Custom emoji win outright: a
 * group that uploaded its own `:tada:` means that one.
 */
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
 * Entries matching `query`, best first: exact, then prefix, then substring,
 * with custom emoji ahead of standard ones at equal tier and shorter names
 * ahead of longer within one. Nothing below `SHORTCODE_MIN_QUERY`.
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
 * Replace `[start, end)` with `insertText`. As in Slack, tapping a suggestion
 * adds a trailing space; typing the closing colon yourself does not.
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

/**
 * One entry per (alias, emoji) pair, so `:+1` and `:thumbsup` match separately.
 * A function because it is built against the stored skin tone.
 */
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
