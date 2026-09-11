/**
 * Localized emoji names and search keywords (#901).
 *
 * The generated table's `name` is the English Unicode name, so on its own the
 * picker only understood English queries even once its chrome was translated.
 * CLDR ships a per-locale name and keyword list for every emoji; the generator
 * vendors them (see `scripts/generate-emoji-data.py`) as one lazily-imported
 * module per locale under `annotations/`, and this module is the shape the
 * picker consumes them in.
 *
 * Type-only imports on purpose, like `emojiShortcodeQuery.ts`: it keeps the
 * module out of the startup chunk and lets `node --test` load it directly.
 */

import type { StandardEmoji } from "./emojiData";
import type { EmojiAnnotationRow } from "./annotations/index";

export interface EmojiAnnotation {
  /** The locale's display name, lowercase (CLDR `tts`). */
  readonly name: string;
  /** Lowercase search keywords, the name excluded. */
  readonly keywords: readonly string[];
}

/** One locale's table, keyed by the bare emoji character. */
export type EmojiAnnotations = ReadonlyMap<string, EmojiAnnotation>;

/**
 * The tables to search, most specific first: the active locale's, then
 * English's, so an English query keeps working for a bilingual user — the
 * same rule the page-keyword catalogues follow.
 */
export type EmojiAnnotationStack = readonly EmojiAnnotations[];

export const NO_ANNOTATIONS: EmojiAnnotationStack = [];

export function buildEmojiAnnotations(rows: readonly EmojiAnnotationRow[]): EmojiAnnotations {
  const table = new Map<string, EmojiAnnotation>();
  for (const [char, name, keywords] of rows) {
    table.set(char, { name, keywords: keywords === "" ? [] : keywords.split("|") });
  }
  return table;
}

/**
 * The name to show for an emoji: the first table that knows it wins, and the
 * Unicode name is the fallback for a locale with no table (or one not loaded
 * yet).
 */
export function emojiDisplayName(emoji: StandardEmoji, tables: EmojiAnnotationStack): string {
  for (const table of tables) {
    const hit = table.get(emoji.char);
    if (hit) {
      return hit.name;
    }
  }
  return emoji.name;
}
