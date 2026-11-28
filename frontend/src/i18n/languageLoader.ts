/**
 * The one way the desktop changes language (#2186). English is bundled; every other language is a
 * local chunk loaded on demand. A chunk that fails or hangs must never stop start-up or sign-in (the
 * catch-up sweep and the outbox flush run only after both), so nothing here ever rejects: a load is
 * bounded by a timeout, and on any failure the app shows English.
 */

import type { i18n as I18n } from "i18next";

/** A language's catalogues, keyed by namespace; empty when it has no chunk. */
export type LoadLanguage = (language: string) => Promise<Record<string, Record<string, unknown>>>;

/** A local chunk loads in milliseconds; this only bounds a broken one. */
const LANGUAGE_LOAD_TIMEOUT_MS = 3000;

export interface LanguageLoaderOptions {
  fallbackLanguage: string;
  timeoutMs?: number;
  warn?: (message: string, error?: unknown) => void;
}

export interface LanguageLoader {
  /** Load `language`'s catalogues once; true when they are present. Never rejects. */
  ensureLanguage(language: string): Promise<boolean>;
  /** Load, then switch to `language`, or to the fallback if it could not load. Never rejects. */
  switchLanguage(language: string): Promise<void>;
}

function withTimeout<T>(promise: Promise<T>, ms: number): Promise<T> {
  return new Promise<T>((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error(`timed out after ${ms} ms`)), ms);
    promise.then(
      (value) => {
        clearTimeout(timer);
        resolve(value);
      },
      (error) => {
        clearTimeout(timer);
        reject(error);
      },
    );
  });
}

export function createLanguageLoader(
  i18n: I18n,
  load: LoadLanguage,
  options: LanguageLoaderOptions,
): LanguageLoader {
  const timeoutMs = options.timeoutMs ?? LANGUAGE_LOAD_TIMEOUT_MS;
  const warn = options.warn ?? ((message, error) => console.warn(message, error));
  // One load per language at a time; a failed one is forgotten, so the next switch tries again.
  const inflight = new Map<string, Promise<boolean>>();

  // Bundled (English, a test locale) or already loaded.
  const isPresent = (language: string): boolean =>
    Object.keys(i18n.store?.data?.[language] ?? {}).length > 0;

  const ensureLanguage = (language: string): Promise<boolean> => {
    if (isPresent(language)) {
      return Promise.resolve(true);
    }
    const running = inflight.get(language);
    if (running) {
      return running;
    }
    const attempt = (async () => {
      try {
        const bundles = await withTimeout(
          Promise.resolve().then(() => load(language)),
          timeoutMs,
        );
        for (const [namespace, catalogue] of Object.entries(bundles)) {
          i18n.addResourceBundle(language, namespace, catalogue, true, false);
        }
        return isPresent(language);
      } catch (error) {
        warn(`[i18n] could not load "${language}"; showing English`, error);
        return false;
      } finally {
        inflight.delete(language);
      }
    })();
    inflight.set(language, attempt);
    return attempt;
  };

  const switchLanguage = async (language: string): Promise<void> => {
    try {
      const loaded = await ensureLanguage(language);
      // Even for the current language: `resolvedLanguage` was fixed before its bundle existed, and
      // dates read it, so it must be re-resolved now (an Arabic UI with English dates otherwise).
      await i18n.changeLanguage(loaded ? language : options.fallbackLanguage);
    } catch (error) {
      warn(`[i18n] could not switch to "${language}"`, error);
    }
  };

  return { ensureLanguage, switchLanguage };
}
