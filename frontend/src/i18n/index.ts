/**
 * i18next setup. Import this module once, near the app root, before any
 * component that calls `useTranslation` renders.
 *
 * Language resolution order on boot:
 *   1. the device-local choice for this device (see `storage.ts`)
 *   2. the OS/browser locale, if we ship a catalogue for it
 *   3. English
 *
 * See `README.md` in this directory for the key conventions and for how to
 * add a locale.
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
 * Shape a Playwright spec puts on `window.__POLLIS_TEST_LOCALE__` (via
 * `page.addInitScript`) to inject a synthetic locale.
 *
 * The catalogues are deliberately allowed to be PARTIAL: a namespace that
 * omits a key is how the suite proves the English fallback, and an omitted
 * namespace is how it proves a whole missing catalogue still renders English.
 */
interface TestLocale extends LanguageOption {
  catalogues: Record<string, Record<string, unknown>>;
}

/**
 * Register the injected test locale before `i18n.init` reads `supportedLngs`.
 * A no-op outside Playwright — `registerTestLanguage` self-guards, and the
 * whole block is dead code in the real bundle.
 */
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

/**
 * The language to start in: a stored device choice wins over the OS guess,
 * and English catches everything else.
 */
export function resolveInitialLanguage(): string {
  const stored = loadDeviceLanguage(null);
  if (stored) {
    return stored;
  }
  return resolveOsLanguage(osLanguageCandidates());
}

/**
 * Keep the document in sync with the active language.
 *
 * `lang` drives the browser's hyphenation, spellcheck dictionary and
 * screen-reader voice; `dir` is what an RTL locale needs (no such locale
 * ships yet, but wiring it here means adding one is a catalogue change and
 * not a plumbing change).
 */
function applyDocumentLanguage(language: string): void {
  try {
    document.documentElement.lang = language;
    document.documentElement.dir = languageDirection(language);
  } catch {
    // No document (non-DOM context) — nothing to sync.
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
  // A key present in `en` but missing in the active language must render the
  // English string. That is `fallbackLng` doing its job, but only if empty
  // strings are not treated as translations — otherwise a stub entry left as
  // "" in a partial catalogue renders as blank UI instead of falling back.
  returnEmptyString: false,
  returnNull: false,
  interpolation: {
    // React escapes everything it renders; escaping again turns apostrophes
    // into `&#39;` in the DOM text.
    escapeValue: false,
  },
  react: {
    useSuspense: false,
  },
});

applyDocumentLanguage(i18n.language);
i18n.on("languageChanged", applyDocumentLanguage);

/**
 * Switch language and remember the choice on this device.
 *
 * `userId` scopes the stored value so two Pollis users sharing one OS account
 * keep separate choices, mirroring the device-local font size.
 */
export async function setLanguage(
  language: string,
  userId?: string | null,
): Promise<void> {
  const normalized = normalizeLanguage(language) ?? DEFAULT_LANGUAGE;
  saveDeviceLanguage(userId ?? null, normalized);
  await i18n.changeLanguage(normalized);
}

/**
 * Re-resolve the language for a user who just signed in. Their stored choice
 * (if any) wins over whatever the pre-auth screens were showing.
 */
export async function adoptUserLanguage(userId: string | null | undefined): Promise<void> {
  const stored = loadDeviceLanguage(userId);
  if (stored && stored !== i18n.language) {
    await i18n.changeLanguage(stored);
  }
}

export default i18n;
