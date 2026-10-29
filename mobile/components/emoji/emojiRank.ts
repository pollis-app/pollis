/**
 * The picker's search ranking, as a pure function so `node --test` can run it
 * against the real generated tables (type-only imports). The desktop copy
 * differs only in the `CustomEmoji` import (`scripts/i18n-check.mjs` fails
 * when anything else drifts).
 */

import type { CustomEmoji } from "../../hooks/queries/useEmoji";
import type { StandardEmoji } from "./emojiData";
import type { EmojiAnnotations, EmojiAnnotationStack } from "./emojiAnnotations";

export type PickerEmoji =
  | { kind: "standard"; emoji: StandardEmoji }
  | { kind: "custom"; emoji: CustomEmoji };

/**
 * Match tiers, lower is better. Names outrank keywords at the same tier, so
 * "cat" surfaces 🐈 (named "cat") above every emoji merely tagged with it.
 */
const NAME_EXACT = 0;
const NAME_PREFIX = 1;
const KEYWORD_EXACT = 2;
const KEYWORD_PREFIX = 3;
const SUBSTRING = 4;

interface Match {
  tier: number;
  length: number;
}

// Combining marks in the scripts we ship, spelled as ranges rather than
// `\p{M}` so the same source runs on Hermes.
const COMBINING_MARKS =
  /[\u0300-\u036f\u0483-\u0489\u0591-\u05bd\u05bf\u05c1\u05c2\u05c4\u05c5\u05c7\u0610-\u061a\u064b-\u065f\u0670\u06d6-\u06dc\u06df-\u06e4\u06e7\u06e8\u06ea-\u06ed\u1ab0-\u1aff\u1dc0-\u1dff\u20d0-\u20ff\ufe20-\ufe2f]/g;

/**
 * Fold a string for matching: lowercase, then strip diacritics, so `cafe`
 * finds "café". Applied to both needle and haystack, so it only has to be
 * consistent, not exact.
 */
export function foldForSearch(value: string): string {
  return value.toLowerCase().normalize("NFD").replace(COMBINING_MARKS, "");
}

interface FoldedAnnotation {
  name: string;
  keywords: readonly string[];
}

/** Tables folded once on first search and kept as long as the table lives (~20k strings). */
const foldedTables = new WeakMap<EmojiAnnotations, Map<string, FoldedAnnotation>>();

function folded(table: EmojiAnnotations): Map<string, FoldedAnnotation> {
  let cached = foldedTables.get(table);
  if (!cached) {
    cached = new Map();
    for (const [char, annotation] of table) {
      cached.set(char, {
        name: foldForSearch(annotation.name),
        keywords: annotation.keywords.map(foldForSearch),
      });
    }
    foldedTables.set(table, cached);
  }
  return cached;
}

function matchName(haystack: string, needle: string): Match | null {
  const index = haystack.indexOf(needle);
  if (index < 0) {
    return null;
  }
  if (haystack === needle) {
    return { tier: NAME_EXACT, length: haystack.length };
  }
  return { tier: index === 0 ? NAME_PREFIX : SUBSTRING, length: haystack.length };
}

function matchKeyword(keyword: string, needle: string): Match | null {
  const index = keyword.indexOf(needle);
  if (index < 0) {
    return null;
  }
  if (keyword === needle) {
    return { tier: KEYWORD_EXACT, length: keyword.length };
  }
  return { tier: index === 0 ? KEYWORD_PREFIX : SUBSTRING, length: keyword.length };
}

function better(a: Match | null, b: Match | null): Match | null {
  if (!a) {
    return b;
  }
  if (!b) {
    return a;
  }
  if (a.tier !== b.tier) {
    return a.tier < b.tier ? a : b;
  }
  return a.length <= b.length ? a : b;
}

function matchStandard(
  emoji: StandardEmoji,
  needle: string,
  tables: EmojiAnnotationStack,
): Match | null {
  let best = matchName(emoji.name, needle);
  for (const table of tables) {
    const annotation = folded(table).get(emoji.char);
    if (!annotation) {
      continue;
    }
    best = better(best, matchName(annotation.name, needle));
    for (const keyword of annotation.keywords) {
      best = better(best, matchKeyword(keyword, needle));
    }
  }
  return best;
}

/**
 * A custom shortcode has only name tiers; a substring hit on it is lifted to
 * the keyword-exact tier, so in a server full of custom emoji they win the
 * tie. It still loses to a standard emoji NAMED that.
 */
function matchCustom(emoji: CustomEmoji, needle: string): Match | null {
  const match = matchName(emoji.shortcode.toLowerCase(), needle);
  if (match && match.tier === SUBSTRING) {
    return { tier: KEYWORD_EXACT, length: match.length };
  }
  return match;
}

/**
 * Rank `query` against the emoji set: exact name, name prefix, exact keyword,
 * keyword prefix, then any substring; within a tier the shorter haystack wins.
 * A standard emoji matches on its Unicode name plus every table in `tables`
 * (the active locale's and English's). Custom emoji rank ahead of standard
 * ones at equal tier.
 */
export function rankEmoji(
  query: string,
  standard: readonly StandardEmoji[],
  custom: readonly CustomEmoji[],
  tables: EmojiAnnotationStack,
): PickerEmoji[] {
  const needle = foldForSearch(query.trim());
  if (!needle) {
    return [];
  }

  const scored: { item: PickerEmoji; rank: number; length: number }[] = [];

  for (const emoji of custom) {
    const match = matchCustom(emoji, needle);
    if (match) {
      scored.push({ item: { kind: "custom", emoji }, rank: match.tier * 2, length: match.length });
    }
  }
  for (const emoji of standard) {
    const match = matchStandard(emoji, needle, tables);
    if (match) {
      scored.push({
        item: { kind: "standard", emoji },
        rank: match.tier * 2 + 1,
        length: match.length,
      });
    }
  }

  scored.sort((a, b) => a.rank - b.rank || a.length - b.length);
  return scored.map((s) => s.item);
}
