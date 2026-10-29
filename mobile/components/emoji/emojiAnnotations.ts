/**
 * Localized emoji names and search keywords (#901), from the CLDR tables the
 * generator vendors under `annotations/` (`scripts/generate-emoji-data.py`).
 * Type-only imports keep it out of the startup chunk and loadable by
 * `node --test`. Byte-identical to desktop's copy (`scripts/i18n-check.mjs`
 * fails when the two drift).
 */

import type { StandardEmoji } from "./emojiData";
import type { EmojiAnnotationRow } from "./annotations/index";

export interface EmojiAnnotation {
  /** The locale's display name, lowercase (CLDR `tts`). */
  readonly name: string;
  /** Lowercase search keywords, the name excluded. */
  readonly keywords: readonly string[];
}

export type EmojiAnnotations = ReadonlyMap<string, EmojiAnnotation>;

/** Tables to search, most specific first: the active locale's, then English's. */
export type EmojiAnnotationStack = readonly EmojiAnnotations[];

export const NO_ANNOTATIONS: EmojiAnnotationStack = [];

export function buildEmojiAnnotations(rows: readonly EmojiAnnotationRow[]): EmojiAnnotations {
  const table = new Map<string, EmojiAnnotation>();
  for (const [char, name, keywords] of rows) {
    table.set(char, { name, keywords: keywords === "" ? [] : keywords.split("|") });
  }
  return table;
}

/** The first table that knows the emoji wins; the Unicode name is the fallback. */
export function emojiDisplayName(emoji: StandardEmoji, tables: EmojiAnnotationStack): string {
  for (const table of tables) {
    const hit = table.get(emoji.char);
    if (hit) {
      return hit.name;
    }
  }
  return emoji.name;
}
