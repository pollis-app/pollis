/**
 * The picker's search ranking, as a pure function over its inputs.
 *
 * Split out of `emojiSearch.ts` so it can be unit-tested under `node --test`
 * against the real generated tables: this module has type-only imports, and
 * the test hands it `STANDARD_EMOJI` and a locale's annotation table itself.
 * The mobile app carries a copy differing only in the `CustomEmoji` import
 * (`scripts/i18n-check.mjs` fails when anything else drifts) — edit this one
 * and copy it across.
 */

import type { CustomEmoji } from "../../hooks/queries/useEmoji";
import type { StandardEmoji } from "./emojiData";
import type { EmojiAnnotations, EmojiAnnotationStack } from "./emojiAnnotations";

/** A picker cell: either a Unicode emoji or a custom per-group one. */
export type PickerEmoji =
  | { kind: "standard"; emoji: StandardEmoji }
  | { kind: "custom"; emoji: CustomEmoji };

/**
 * How well a haystack matches, lower is better. Names (the Unicode name, a
 * localized name, a custom shortcode) outrank keywords at the same tier, so
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

// Combining marks in the scripts we ship (Latin, Cyrillic, Hebrew, Arabic,
// plus the general combining blocks), spelled as ranges rather than `\p{M}`
// so the same source runs on Hermes.
const COMBINING_MARKS =
  /[\u0300-\u036f\u0483-\u0489\u0591-\u05bd\u05bf\u05c1\u05c2\u05c4\u05c5\u05c7\u0610-\u061a\u064b-\u065f\u0670\u06d6-\u06dc\u06df-\u06e4\u06e7\u06e8\u06ea-\u06ed\u1ab0-\u1aff\u1dc0-\u1dff\u20d0-\u20ff\ufe20-\ufe2f]/g;

/**
 * Fold a string for matching: lowercase, then strip diacritics, so `corazon`
 * finds "corazón" and `cafe` finds "café". Phones without dead keys make the
 * unaccented spelling the common one, not the exception. Applied to both the
 * needle and every haystack, so the fold never has to be exact — only
 * consistent.
 */
export function foldForSearch(value: string): string {
  return value.toLowerCase().normalize("NFD").replace(COMBINING_MARKS, "");
}

interface FoldedAnnotation {
  name: string;
  keywords: readonly string[];
}

/**
 * A table's names and keywords folded once, on the first search against it,
 * and kept for as long as the table itself lives. Folding ~20k short strings
 * per keystroke would be felt; folding them once per language is not.
 */
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

/**
 * The best match for a standard emoji across its Unicode name and every
 * annotation table in the stack.
 */
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
 * A custom shortcode has no keywords, so its only tiers are the name ones. A
 * substring hit on it is lifted to the keyword-exact tier: someone who typed
 * three letters in a server full of custom emoji is far more likely to mean
 * one of those than an emoji merely tagged with the word, and the custom
 * bonus then wins the tie. It still loses to a standard emoji NAMED that.
 */
function matchCustom(emoji: CustomEmoji, needle: string): Match | null {
  const match = matchName(emoji.shortcode.toLowerCase(), needle);
  if (match && match.tier === SUBSTRING) {
    return { tier: KEYWORD_EXACT, length: match.length };
  }
  return match;
}

/**
 * Rank `query` against the emoji set.
 *
 * Tiers, in the order a person expects: an exact name match, a name prefix, an
 * exact keyword, a keyword prefix, then any substring. Within a tier the
 * shorter haystack wins — searching "cat" should surface "cat" above "cat with
 * wry smile". A standard emoji is matched on its Unicode name plus every table
 * in `tables` (the active locale's and English's), so `corazón`, `corazon`
 * and `heart` all find ❤ for a Spanish user (diacritics are folded on both
 * sides — see `foldForSearch`).
 *
 * Custom emoji rank ahead of standard ones at equal tier, and a custom
 * substring hit counts as a keyword-exact one (`matchCustom`).
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
