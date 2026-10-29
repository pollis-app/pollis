/** Localized emoji names/keywords from CLDR (#901). Type-only imports. Mobile has a byte-identical copy (checked by scripts/i18n-check.mjs): edit here, copy across. */

import type { StandardEmoji } from "./emojiData";
import type { EmojiAnnotationRow } from "./annotations/index";

export interface EmojiAnnotation {
  readonly name: string;
  readonly keywords: readonly string[];
}

export type EmojiAnnotations = ReadonlyMap<string, EmojiAnnotation>;

/** Active locale's table first, then English's. */
export type EmojiAnnotationStack = readonly EmojiAnnotations[];

export const NO_ANNOTATIONS: EmojiAnnotationStack = [];

export function buildEmojiAnnotations(rows: readonly EmojiAnnotationRow[]): EmojiAnnotations {
  const table = new Map<string, EmojiAnnotation>();
  for (const [char, name, keywords] of rows) {
    table.set(char, { name, keywords: keywords === "" ? [] : keywords.split("|") });
  }
  return table;
}

/** First table that knows it wins; else the Unicode name. */
export function emojiDisplayName(emoji: StandardEmoji, tables: EmojiAnnotationStack): string {
  for (const table of tables) {
    const hit = table.get(emoji.char);
    if (hit) {
      return hit.name;
    }
  }
  return emoji.name;
}
