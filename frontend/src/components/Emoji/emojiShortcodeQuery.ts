/**
 * `:shortcode:` parsing and ranking for the composer (#848 follow-up).
 *
 * Pure and dependency-free — no React, and deliberately no import of
 * `emojiData.ts`, which is the largest module in the bundle and is code-split
 * behind the picker's lazy chunk (#874). Everything here works on a flat list
 * of `ShortcodeEntry`, which the caller assembles: the custom half from the
 * `list_usable_emoji` query, the standard half from `emojiShortcodeIndex.ts`,
 * which is the only module that pulls the table in and is imported on demand.
 *
 * Two grammars live here and they are not the same:
 *
 *   - `shortcodeQueryAt` — an UNTERMINATED `:jo` the caret is sitting inside,
 *     which drives the suggestion list and the terminal ghost;
 *   - `completedShortcodeAt` — a closed `:joy:`, which the composer substitutes
 *     outright the moment the user types the second colon, Slack-style.
 *
 * Both open only at the start of the string or after whitespace, which is what
 * keeps `http://example.com` and `10:30` from ever reading as a shortcode.
 */

import type { StandardEmoji } from "./emojiData";

/**
 * Characters that may appear inside a shortcode after the opening ':'.
 *
 * WIDER than a custom emoji shortcode on purpose: the standard set vendored
 * from gemoji contains `+` and `-` (`:+1:`, `:-1:`, `:e-mail:`). This does NOT
 * widen what a custom shortcode may be — that is still `[a-z0-9_]{2,32}`,
 * enforced by `SHORTCODE_RE`, pollis-core and the DS. A query containing `+`
 * or `-` simply cannot match a custom emoji, because none can contain one.
 */
const SHORTCODE_BODY = /[A-Za-z0-9_+-]/;

/** Shortest query that opens the suggestion list. Discord's rule. */
export const SHORTCODE_MIN_QUERY = 2;

/** Max rows offered at once, matching the mention list's depth. */
export const SHORTCODE_SUGGESTION_LIMIT = 8;

/** One thing a `:shortcode:` can resolve to, custom or standard. */
export interface ShortcodeEntry {
  /** The alias itself, lowercase, without the colons. */
  shortcode: string;
  /** What gets written into the composer — a character, or a wire token. */
  insertText: string;
  /**
   * True for a group's custom emoji. Custom wins every tie: a group that
   * named its own `:tada:` means that one.
   */
  custom: boolean;
  /** Standard emoji: the character to show in the row. */
  char: string | null;
  /** Custom emoji: the content hash `CustomEmojiImage` resolves. */
  contentHash: string | null;
  /** Secondary label — the Unicode name, or the owning group's name. */
  label: string;
}

/** An in-progress `:…` the caret is sitting inside. */
export interface ShortcodeQuery {
  /** Index of the opening ':'. */
  start: number;
  /** Caret index — always the end of the query. */
  end: number;
  /** Text typed after the ':', lowercased. */
  query: string;
}

/**
 * True when a ':' at `index` is allowed to open a shortcode: string start, or
 * straight after whitespace. This single rule is what makes `http://` and
 * `10:30` inert, and it is shared by both grammars below.
 */
function opensAt(text: string, index: number): boolean {
  return index === 0 || /\s/.test(text[index - 1]);
}

/**
 * The shortcode the caret is currently typing, or null.
 *
 * Mirrors `mentionQueryAt`: only an unterminated run of body characters
 * immediately before the caret counts, so the suggestion disappears the moment
 * the user types a space.
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
  /** Index of the opening ':'. */
  start: number;
  /** Index one past the closing ':' — i.e. the caret. */
  end: number;
  /** The name between the colons, lowercased. */
  name: string;
}

/**
 * The `:name:` that ends at `caret`, or null.
 *
 * There is no minimum length here — `:v:` and `:o:` are real gemoji aliases.
 * The two-character floor is `SHORTCODE_MIN_QUERY`, and it governs when the
 * list opens, not what a valid shortcode is.
 */
export function completedShortcodeAt(text: string, caret: number): CompletedShortcode | null {
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
 * The entry a completed `:name:` resolves to, or undefined.
 *
 * Custom emoji win outright, which is the whole point: a group that uploaded
 * its own `:tada:` must get its own image, not the Unicode party popper.
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
 * Entries matching `query`, best first.
 *
 * Three tiers in the order a person expects — exact, then prefix, then
 * substring — with custom emoji ahead of standard ones at equal tier, which is
 * the same rule `searchEmoji` applies in the picker. Shorter shortcodes win
 * within a tier, so ":sm" offers `:smile:` before `:smiley_cat:`; the alias
 * itself breaks the remaining ties so the order never depends on input order.
 *
 * Returns nothing below `SHORTCODE_MIN_QUERY` characters: one letter after a
 * colon matches hundreds of emoji and is far more likely to be prose.
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
 * Replace the slice `[start, end)` of `text` with `insertText`, returning the
 * new text and where the caret should land.
 *
 * `trailingSpace` is the difference between the two acceptance paths, and it
 * matches Slack: picking from the list finishes a word, so a space follows and
 * the user keeps typing; typing the closing colon yourself is mid-word, so
 * nothing is added.
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

/** Build the composer entry for one standard emoji alias. */
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
