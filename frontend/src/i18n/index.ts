/** i18next setup; import once at the root. Language: device choice, then OS locale, then English. */

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

/** Playwright's `window.__POLLIS_TEST_LOCALE__`; catalogues may be partial to test fallback. */
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

export function resolveInitialLanguage(): string {
  const stored = loadDeviceLanguage(null);
  if (stored) {
    return stored;
  }
  return resolveOsLanguage(osLanguageCandidates());
}

/** Sync `lang` and `dir` on the document. */
function applyDocumentLanguage(language: string): void {
  try {
    document.documentElement.lang = language;
    document.documentElement.dir = languageDirection(language);
  } catch {
  }
}

/** Shipped catalogues, plus the injected test locale's partial ones. */
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
  // Empty strings fall back to English instead of rendering blank.
  returnEmptyString: false,
  returnNull: false,
  interpolation: {
    // React already escapes.
    escapeValue: false,
  },
  react: {
    useSuspense: false,
  },
});

applyDocumentLanguage(i18n.language);
i18n.on("languageChanged", applyDocumentLanguage);

/** Switch language and store it per user on this device. */
export async function setLanguage(
  language: string,
  userId?: string | null,
): Promise<void> {
  const normalized = normalizeLanguage(language) ?? DEFAULT_LANGUAGE;
  saveDeviceLanguage(userId ?? null, normalized);
  await i18n.changeLanguage(normalized);
}

/** A signed-in user's stored choice wins over the pre-auth language. */
export async function adoptUserLanguage(userId: string | null | undefined): Promise<void> {
  const stored = loadDeviceLanguage(userId);
  if (stored && stored !== i18n.language) {
    await i18n.changeLanguage(stored);
  }
}

export default i18n;
