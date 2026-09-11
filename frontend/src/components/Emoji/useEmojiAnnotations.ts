/**
 * The annotation tables the picker should search and label with, for the
 * active language: that locale's table first, then English's.
 *
 * Each table is a dynamic import (#874's rule — the tables are as large as
 * `emojiData.ts` itself and only two are ever needed), resolved the first time
 * a language is asked for and cached for the session after that. Until it
 * resolves the picker searches Unicode names alone, which is exactly the
 * pre-#901 behaviour and never a broken state.
 */

import { useEffect, useState } from "react";
import { DEFAULT_LANGUAGE, normalizeLanguage } from "../../i18n/languages";
import { loadEmojiAnnotationRows } from "./annotations/index";
import {
  buildEmojiAnnotations,
  NO_ANNOTATIONS,
  type EmojiAnnotations,
  type EmojiAnnotationStack,
} from "./emojiAnnotations";

const cache = new Map<string, Promise<EmojiAnnotations | null>>();

function loadTable(locale: string): Promise<EmojiAnnotations | null> {
  let pending = cache.get(locale);
  if (!pending) {
    const rows = loadEmojiAnnotationRows(locale);
    pending = rows
      ? rows.then(buildEmojiAnnotations).catch(() => {
          // A failed chunk load costs localized search and nothing else; drop
          // the entry so the next picker open can try again.
          cache.delete(locale);
          return null;
        })
      : Promise.resolve(null);
    cache.set(locale, pending);
  }
  return pending;
}

/** Active locale's table, then English's, deduplicated and skipping any that failed. */
export async function loadEmojiAnnotationStack(language: string): Promise<EmojiAnnotationStack> {
  const locale = normalizeLanguage(language) ?? DEFAULT_LANGUAGE;
  const locales = locale === DEFAULT_LANGUAGE ? [locale] : [locale, DEFAULT_LANGUAGE];
  const tables = await Promise.all(locales.map(loadTable));
  return tables.filter((table): table is EmojiAnnotations => table !== null);
}

export function useEmojiAnnotations(language: string): EmojiAnnotationStack {
  const [stack, setStack] = useState<EmojiAnnotationStack>(NO_ANNOTATIONS);

  useEffect(() => {
    let cancelled = false;
    void loadEmojiAnnotationStack(language).then((loaded) => {
      if (!cancelled) {
        setStack(loaded);
      }
    });
    return () => {
      cancelled = true;
    };
  }, [language]);

  return stack;
}
