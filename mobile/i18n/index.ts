/**
 * i18next setup for mobile. Import this module once, from the root layout,
 * before any screen that calls `useTranslation` renders.
 *
 * Language resolution order on boot, matching desktop:
 *   1. the device-local choice on this device (`storage.ts`, async — see
 *      `hydrateLanguage`, which the root layout awaits behind the splash)
 *   2. the device locale, if we ship a catalogue for it
 *   3. English
 *
 * The catalogues are the desktop's own files (`resources.ts`), so a key is
 * addressed exactly as it is there: `t("settings:language.heading")`, or
 * `t("language.heading")` under `useTranslation("settings")`. Copy that
 * exists only on mobile lives in the `mobile` namespace.
 */

import { I18nManager } from "react-native";
import i18n from "i18next";
import { initReactI18next } from "react-i18next";

import { RESOURCES } from "./resources";
import { NAMESPACES, DEFAULT_NAMESPACE } from "./namespaces";
import {
  DEFAULT_LANGUAGE,
  deviceLanguageCandidates,
  languageDirection,
  localeUpperCase,
  normalizeLanguage,
  resolveDeviceLanguage,
  supportedLanguageCodes,
} from "./languages";
import { loadDeviceLanguage, saveDeviceLanguage } from "./storage";

void i18n.use(initReactI18next).init({
  resources: RESOURCES,
  lng: resolveDeviceLanguage(deviceLanguageCandidates()),
  fallbackLng: DEFAULT_LANGUAGE,
  supportedLngs: supportedLanguageCodes(),
  ns: NAMESPACES,
  defaultNS: DEFAULT_NAMESPACE,
  // A key present in `en` but missing in the active language must render the
  // English string, so an empty stub must not count as a translation.
  returnEmptyString: false,
  returnNull: false,
  interpolation: {
    // React Native renders strings verbatim — nothing to escape for.
    escapeValue: false,
  },
  react: {
    useSuspense: false,
  },
});

/**
 * The locale every `toLocale*` / `Intl.*` call must format against — the one
 * i18next actually selected, never a bare `undefined`, which resolves to the
 * host locale and puts an English date under an Arabic heading.
 */
export function activeLocale(): string {
  return i18n.resolvedLanguage || i18n.language || DEFAULT_LANGUAGE;
}

/**
 * Uppercase translated copy for the label style (`SELF`, `PREFERENCES`,
 * `ACCENT`). The one sanctioned spelling — bare `toUpperCase()` is the
 * locale-invariant mapping, which is wrong in Turkish and a no-op in Arabic.
 */
export function upper(value: string): string {
  return localeUpperCase(value, activeLocale());
}

/**
 * Whether the native layout direction is still the one the app was launched
 * with while the language now wants the other. React Native applies
 * `forceRTL` on the NEXT launch, so a switch between an LTR and an RTL
 * language leaves the layout one step behind until the app restarts.
 */
export function layoutRestartPending(): boolean {
  return I18nManager.isRTL !== (languageDirection(i18n.language) === "rtl");
}

/**
 * React Native computes `isRTL = forced || (allowed && deviceLocaleIsRTL)`,
 * so `forceRTL(false)` alone can never un-mirror an Arabic-locale phone whose
 * user picked English: `allowRTL` has to follow the language too. Both are
 * persisted natively and read at the next launch.
 */
function applyLayoutDirection(language: string): void {
  const wantRtl = languageDirection(language) === "rtl";
  I18nManager.allowRTL(wantRtl);
  I18nManager.forceRTL(wantRtl);
}

applyLayoutDirection(i18n.language);

/**
 * Apply the stored device choice, if any. Resolves once the language on
 * screen is the one the user last picked; the root layout holds the splash
 * until then so the first frame is never English-then-flicker.
 */
export async function hydrateLanguage(): Promise<void> {
  const stored = await loadDeviceLanguage(null);
  if (stored && stored !== i18n.language) {
    await i18n.changeLanguage(stored);
  }
  applyLayoutDirection(i18n.language);
}

/** Switch language and remember the choice on this device (and for `userId`). */
export async function setLanguage(language: string, userId?: string | null): Promise<void> {
  const normalized = normalizeLanguage(language) ?? DEFAULT_LANGUAGE;
  await saveDeviceLanguage(userId ?? null, normalized);
  await i18n.changeLanguage(normalized);
  applyLayoutDirection(normalized);
}

/** Re-resolve for a user who just signed in: their stored choice wins. */
export async function adoptUserLanguage(userId: string | null | undefined): Promise<void> {
  const stored = await loadDeviceLanguage(userId);
  if (stored && stored !== i18n.language) {
    await i18n.changeLanguage(stored);
    applyLayoutDirection(stored);
  }
}

export default i18n;
