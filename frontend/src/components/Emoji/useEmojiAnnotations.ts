/** Active locale's annotation table then English's, each a dynamic import, cached per language. Mobile has a byte-identical copy (checked by scripts/i18n-check.mjs): edit here, copy across. */

import { useEffect, useState } from "react";
import { DEFAULT_LANGUAGE, normalizeLanguage } from "../../i18n/languages";
import { loadEmojiAnnotationRows } from "./annotations/index";
import {
  buildEmojiAnnotations,
  NO_ANNOTATIONS,
  type EmojiAnnotations,
  type EmojiAnnotationStack,
} from "./emojiAnnotations";

const pendingTables = new Map<string, Promise<EmojiAnnotations | null>>();
const resolvedStacks = new Map<string, EmojiAnnotationStack>();

function loadTable(locale: string): Promise<EmojiAnnotations | null> {
  let pending = pendingTables.get(locale);
  if (!pending) {
    const rows = loadEmojiAnnotationRows(locale);
    pending = rows
      ? rows.then(buildEmojiAnnotations).catch(() => {
          // Drop the entry so the next open retries.
          pendingTables.delete(locale);
          return null;
        })
      : Promise.resolve(null);
    pendingTables.set(locale, pending);
  }
  return pending;
}

function stackLocale(language: string): string {
  return normalizeLanguage(language) ?? DEFAULT_LANGUAGE;
}

export async function loadEmojiAnnotationStack(language: string): Promise<EmojiAnnotationStack> {
  const locale = stackLocale(language);
  const cached = resolvedStacks.get(locale);
  if (cached) {
    return cached;
  }
  const locales = locale === DEFAULT_LANGUAGE ? [locale] : [locale, DEFAULT_LANGUAGE];
  const tables = await Promise.all(locales.map(loadTable));
  const stack = tables.filter((table): table is EmojiAnnotations => table !== null);
  if (stack.length === locales.length) {
    resolvedStacks.set(locale, stack);
  }
  return stack;
}

export function useEmojiAnnotations(language: string): EmojiAnnotationStack {
  const [stack, setStack] = useState<EmojiAnnotationStack>(
    () => resolvedStacks.get(stackLocale(language)) ?? NO_ANNOTATIONS,
  );

  useEffect(() => {
    let cancelled = false;
    void loadEmojiAnnotationStack(language).then((loaded) => {
      if (!cancelled) {
        // Same reference: keeps memoised sections intact.
        setStack((current) => (current === loaded ? current : loaded));
      }
    });
    return () => {
      cancelled = true;
    };
  }, [language]);

  return stack;
}
