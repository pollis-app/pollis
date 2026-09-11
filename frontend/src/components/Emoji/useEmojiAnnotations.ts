/**
 * The annotation tables the picker should search and label with, for the
 * active language: that locale's table first, then English's.
 *
 * Each table sits behind a dynamic import. In the web bundle that is a
 * separate chunk (#874's rule — the tables are as large as `emojiData.ts`
 * itself and only two are ever needed); on native, where Metro does not
 * split, every locale ships in the binary and the import defers parsing until
 * the picker opens. Either way a stack is resolved once per language and
 * cached for the session, so re-opening the picker starts from the cached
 * stack synchronously. Until the first resolution the picker searches Unicode
 * names alone, which is exactly the pre-#901 behaviour and never a broken
 * state.
 *
 * The mobile app carries a byte-identical copy (`scripts/i18n-check.mjs`
 * fails when the two drift) — edit this one and copy it across.
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

const pendingTables = new Map<string, Promise<EmojiAnnotations | null>>();
const resolvedStacks = new Map<string, EmojiAnnotationStack>();

function loadTable(locale: string): Promise<EmojiAnnotations | null> {
  let pending = pendingTables.get(locale);
  if (!pending) {
    const rows = loadEmojiAnnotationRows(locale);
    pending = rows
      ? rows.then(buildEmojiAnnotations).catch(() => {
          // A failed load costs localized search and nothing else; drop the
          // entry so the next picker open can try again.
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

/** Active locale's table, then English's, deduplicated and skipping any that failed. */
export async function loadEmojiAnnotationStack(language: string): Promise<EmojiAnnotationStack> {
  const locale = stackLocale(language);
  const cached = resolvedStacks.get(locale);
  if (cached) {
    return cached;
  }
  const locales = locale === DEFAULT_LANGUAGE ? [locale] : [locale, DEFAULT_LANGUAGE];
  const tables = await Promise.all(locales.map(loadTable));
  const stack = tables.filter((table): table is EmojiAnnotations => table !== null);
  // A stack with a failed table is not cached, so a later open retries it.
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
        // Same reference as the cached stack: skipping the set keeps the
        // picker's memoised sections intact on every open after the first.
        setStack((current) => (current === loaded ? current : loaded));
      }
    });
    return () => {
      cancelled = true;
    };
  }, [language]);

  return stack;
}
