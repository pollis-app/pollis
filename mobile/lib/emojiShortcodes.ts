/**
 * `:shortcode:` parsing, resolution and ranking for the mobile composer.
 *
 * MIRROR of `frontend/src/components/Emoji/emojiShortcodeQuery.ts` plus the
 * index half of `emojiShortcodeIndex.ts`. Mobile is a standalone Expo project
 * and cannot import across the boundary, so the two files are kept in step by
 * hand — the same arrangement `lib/mentions.ts` and `components/emoji/` already
 * live under. The desktop copy is split in two because the emoji table is
 * code-split out of the renderer's startup chunk; Metro bundles everything
 * anyway, and the picker already pulls the table in, so here it is one module.
 *
 * Two grammars, and they are not the same:
 *
 *   - `shortcodeQueryAt` — an UNTERMINATED `:jo` the caret is inside, which
 *     drives the suggestion list;
 *   - `completedShortcodeAt` — a closed `:joy:`, substituted outright the
 *     moment the user types the second colon, Slack-style.
 *
 * Both open only at the start of the string or after whitespace, which is what
 * keeps `http://example.com` and `10:30` from ever reading as a shortcode.
 */

import { STANDARD_EMOJI } from "../components/emoji/emojiData";
import { emojiDisplayChar } from "../components/emoji/emojiSearch";
import { emojiTokenText } from "../components/emoji/emojiTokens";
import { readSkinTone } from "./emojiPrefs";
import type { CustomEmoji } from "../hooks/queries/useEmoji";

/**
 * Characters allowed inside a shortcode after the opening ':'.
 *
 * WIDER than a custom emoji shortcode on purpose: the standard set vendored
 * from gemoji contains `+` and `-` (`:+1:`, `:-1:`, `:e-mail:`). A custom
 * shortcode is still `[a-z0-9_]{2,32}`, enforced by pollis-core and the DS, so
 * a query carrying `+` simply cannot match one.
 */
const SHORTCODE_BODY = /[A-Za-z0-9_+-]/;

/** Shortest query that opens the suggestion list. Discord's rule. */
export const SHORTCODE_MIN_QUERY = 2;

/** Max rows offered at once. */
export const SHORTCODE_SUGGESTION_LIMIT = 8;

/** One thing a `:shortcode:` can resolve to, custom or standard. */
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

/** The shortcode the caret is currently typing, or null. */
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
 * Replace `[start, end)` with `insertText`.
 *
 * `trailingSpace` is the difference between the two acceptance paths, and it
 * matches Slack: tapping a suggestion finishes a word, so a space follows;
 * typing the closing colon yourself is mid-word, so nothing is added.
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
 * Every standard alias as a composer entry — one row per (alias, emoji) pair,
 * so `:+1` and `:thumbsup` are separately matchable and insert the same
 * character. Built against the stored skin tone, which is why it is a function.
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

/** Custom emoji as composer entries. Listed first, though nothing depends on it. */
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
