/**
 * The picker's search ranking, as a pure function over its inputs.
 *
 * Split out of `emojiSearch.ts` so it can be unit-tested under `node --test`
 * against the real generated tables: this module has type-only imports, and
 * the test hands it `STANDARD_EMOJI` and a locale's annotation table itself.
 */

import type { CustomEmoji } from "../../types";
import type { StandardEmoji } from "./emojiData";
import type { EmojiAnnotationStack } from "./emojiAnnotations";

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
    const annotation = table.get(emoji.char);
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
 * Rank `query` against the emoji set.
 *
 * Tiers, in the order a person expects: an exact name match, a name prefix, an
 * exact keyword, a keyword prefix, then any substring. Within a tier the
 * shorter haystack wins — searching "cat" should surface "cat" above "cat with
 * wry smile". A standard emoji is matched on its Unicode name plus every table
 * in `tables` (the active locale's and English's), so `corazón` and `heart`
 * both find ❤ for a Spanish user.
 *
 * Custom emoji rank ahead of standard ones at equal tier: someone who typed
 * three letters in a server full of custom emoji is far more likely to mean one
 * of those.
 */
export function rankEmoji(
  query: string,
  standard: readonly StandardEmoji[],
  custom: readonly CustomEmoji[],
  tables: EmojiAnnotationStack,
): PickerEmoji[] {
  const needle = query.trim().toLowerCase();
  if (!needle) {
    return [];
  }

  const scored: { item: PickerEmoji; rank: number; length: number }[] = [];

  for (const emoji of custom) {
    const match = matchName(emoji.shortcode.toLowerCase(), needle);
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
