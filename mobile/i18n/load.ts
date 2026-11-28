// Which languages' catalogues i18next holds (#2187, pure): English, the fallback, and the active
// language at launch; another only when the user switches to it. Every language change goes
// through `switchLanguage`, which adds the bundle first, so `t()` and `resolvedLanguage` answer in
// the new language the moment it is active. Synchronous on purpose: a loader is a bundled
// `require`, so nothing here can hang, and no caller (the splash hold, the push channel name)
// has anything to await. A loader that throws leaves English, the fallback, in place.

import type { i18n as I18n } from "i18next";

type Catalogue = Record<string, unknown>;
export type LanguageBundle = Record<string, Catalogue>;
export type LanguageLoaders = Readonly<Record<string, () => LanguageBundle>>;

/** One language's catalogues, or null when it has no loader or its loader failed. */
export function loadLanguage(loaders: LanguageLoaders, language: string): LanguageBundle | null {
  const load = loaders[language];
  if (!load) {
    return null;
  }
  try {
    return load();
  } catch (error) {
    console.warn(`[i18n] catalogues for ${language} failed to load; showing English`, error);
    return null;
  }
}

/** The init resources: English plus the given languages, each built once. */
export function initialResources(
  loaders: LanguageLoaders,
  fallback: string,
  languages: readonly string[],
): Record<string, LanguageBundle> {
  const out: Record<string, LanguageBundle> = {};
  for (const language of [fallback, ...languages]) {
    if (out[language]) {
      continue;
    }
    const bundle = loadLanguage(loaders, language);
    if (bundle) {
      out[language] = bundle;
    }
  }
  return out;
}

/** Add `language`'s catalogues unless i18next already holds them. True once they are there. */
export function ensureLanguage(
  i18n: I18n,
  loaders: LanguageLoaders,
  namespace: string,
  language: string,
): boolean {
  if (i18n.hasResourceBundle(language, namespace)) {
    return true;
  }
  const bundle = loadLanguage(loaders, language);
  if (!bundle) {
    return false;
  }
  for (const [ns, catalogue] of Object.entries(bundle)) {
    i18n.addResourceBundle(language, ns, catalogue, true, false);
  }
  return true;
}

/** The one language change: the catalogues first, then `changeLanguage` (which never waits on a load). */
export async function switchLanguage(
  i18n: I18n,
  loaders: LanguageLoaders,
  namespace: string,
  language: string,
): Promise<void> {
  ensureLanguage(i18n, loaders, namespace, language);
  await i18n.changeLanguage(language);
}
