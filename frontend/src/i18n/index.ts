/**
 * i18next setup; import once near the root, before any `useTranslation`.
 * Boot language: the device-local choice, then a shipped OS locale, then
 * English. See `.codesight/wiki/i18n.md`.
 */

import i18n from "i18next";
import { initReactI18next } from "react-i18next";

import { buildResources } from "./resources";
import { DEFAULT_NAMESPACE, NAMESPACES } from "./namespaces";
import {
  DEFAULT_LANGUAGE,
  languageDirection,
  normalizeLanguage,
  osLanguageCandidates,
  registerTestLanguage,
  resolveOsLanguage,
  supportedLanguageCodes,
  type LanguageOption,
} from "./languages";
import { loadDeviceLanguage, saveDeviceLanguage } from "./storage";

/**
 * What a Playwright spec puts on `window.__POLLIS_TEST_LOCALE__`. Catalogues
 * may be PARTIAL, which is how the suite proves the English fallback.
 */
interface TestLocale extends LanguageOption {
  catalogues: Record<string, Record<string, unknown>>;
}

/** Register the test locale before `i18n.init`; a no-op outside Playwright. */
function installTestLocale(): TestLocale | null {
  if (import.meta.env.VITE_PLAYWRIGHT !== "true") {
    return null;
  }
  const injected = (window as unknown as Record<string, unknown>)
    .__POLLIS_TEST_LOCALE__ as TestLocale | undefined;
  if (!injected?.code) {
    return null;
  }
  registerTestLanguage({
    code: injected.code,
    label: injected.label,
    dir: injected.dir ?? "ltr",
  });
  return injected;
}

const testLocale = installTestLocale();

/** Stored device choice, then the OS guess, then English. */
export function resolveInitialLanguage(): string {
  const stored = loadDeviceLanguage(null);
  if (stored) {
    return stored;
  }
  return resolveOsLanguage(osLanguageCandidates());
}

/** Sync `lang` (hyphenation, spellcheck, screen reader) and `dir` on the document. */
function applyDocumentLanguage(language: string): void {
  try {
    document.documentElement.lang = language;
    document.documentElement.dir = languageDirection(language);
  } catch {
    // No document (non-DOM context) — nothing to sync.
  }
}

function initialResources(): Record<string, Record<string, Record<string, unknown>>> {
  const resources = buildResources();
  if (testLocale) {
    resources[testLocale.code] = testLocale.catalogues ?? {};
  }
  return resources;
}

void i18n.use(initReactI18next).init({
  resources: initialResources(),
  lng: resolveInitialLanguage(),
  fallbackLng: DEFAULT_LANGUAGE,
  supportedLngs: supportedLanguageCodes(),
  ns: NAMESPACES,
  defaultNS: DEFAULT_NAMESPACE,
  // Without this a "" stub in a partial catalogue renders blank instead of
  // falling back to English.
  returnEmptyString: false,
  returnNull: false,
  interpolation: {
    // React escapes already; escaping twice shows `&#39;`.
    escapeValue: false,
  },
  react: {
    useSuspense: false,
  },
});

applyDocumentLanguage(i18n.language);
i18n.on("languageChanged", applyDocumentLanguage);

/** Switch language and remember it on this device, scoped per user. */
export async function setLanguage(
  language: string,
  userId?: string | null,
): Promise<void> {
  const normalized = normalizeLanguage(language) ?? DEFAULT_LANGUAGE;
  saveDeviceLanguage(userId ?? null, normalized);
  await i18n.changeLanguage(normalized);
}

/** Re-resolve for a user who just signed in; their stored choice wins. */
export async function adoptUserLanguage(userId: string | null | undefined): Promise<void> {
  const stored = loadDeviceLanguage(userId);
  if (stored && stored !== i18n.language) {
    await i18n.changeLanguage(stored);
  }
}

export default i18n;
