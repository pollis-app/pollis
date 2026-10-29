/** i18next setup; import once from the root layout. Resolution: stored choice, device locale, English. */

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
  // A key missing in the active language renders English: an empty stub is no translation.
  returnEmptyString: false,
  returnNull: false,
  interpolation: {
    escapeValue: false,
  },
  react: {
    useSuspense: false,
  },
});

/** The locale every `toLocale*` / `Intl.*` call must pass; `undefined` means the host locale. */
export function activeLocale(): string {
  return i18n.resolvedLanguage || i18n.language || DEFAULT_LANGUAGE;
}

/** Uppercase translated copy; bare `toUpperCase()` is wrong in Turkish and a no-op in Arabic. */
export function upper(value: string): string {
  return localeUpperCase(value, activeLocale());
}

/** True until restart after an LTR↔RTL switch: React Native applies `forceRTL` on the NEXT launch. */
export function layoutRestartPending(): boolean {
  return I18nManager.isRTL !== (languageDirection(i18n.language) === "rtl");
}

/** `allowRTL` must follow the language too, or `forceRTL(false)` cannot un-mirror an Arabic-locale phone. */
function applyLayoutDirection(language: string): void {
  const wantRtl = languageDirection(language) === "rtl";
  I18nManager.allowRTL(wantRtl);
  I18nManager.forceRTL(wantRtl);
}

// No `applyLayoutDirection` at module load: Android re-reads it on every root measure, so the device-locale guess would un-mirror the launch.

/** Apply the stored choice; the root layout holds the splash until it resolves. */
export async function hydrateLanguage(): Promise<void> {
  const stored = await loadDeviceLanguage(null);
  if (stored && stored !== i18n.language) {
    await i18n.changeLanguage(stored);
  }
  applyLayoutDirection(i18n.language);
}

export async function setLanguage(language: string, userId?: string | null): Promise<void> {
  const normalized = normalizeLanguage(language) ?? DEFAULT_LANGUAGE;
  await saveDeviceLanguage(userId ?? null, normalized);
  await i18n.changeLanguage(normalized);
  applyLayoutDirection(normalized);
}

export async function adoptUserLanguage(userId: string | null | undefined): Promise<void> {
  const stored = await loadDeviceLanguage(userId);
  if (stored && stored !== i18n.language) {
    await i18n.changeLanguage(stored);
    applyLayoutDirection(stored);
  }
}

export default i18n;
