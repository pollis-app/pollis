/** Mobile mirror of `frontend/src/i18n/languages.ts`; tests/i18n.test.ts fails if it drifts. Pure. */

import { matchLanguageTag } from "./languageTag.ts";

export interface LanguageOption {
  /** Lowercase base tag (`ja`), or `<base>-<Script>` for a language in SCRIPT_VARIANTS (`pa-Arab`); see languageTag.ts. */
  code: string;
  /** The endonym, never the English name. */
  label: string;
  dir: "ltr" | "rtl";
}

export const DEFAULT_LANGUAGE = "en";

export const SUPPORTED_LANGUAGES: readonly LanguageOption[] = [
  { code: "en", label: "English", dir: "ltr" },
  { code: "es", label: "Español", dir: "ltr" },
  { code: "uk", label: "Українська", dir: "ltr" },
  { code: "fr", label: "Français", dir: "ltr" },
  { code: "ru", label: "Русский", dir: "ltr" },
  { code: "zh", label: "简体中文", dir: "ltr" },
  { code: "ar", label: "العربية", dir: "rtl" },
];

export function supportedLanguageCodes(): string[] {
  return SUPPORTED_LANGUAGES.map((l) => l.code);
}

export function languageOption(code: string): LanguageOption | undefined {
  return SUPPORTED_LANGUAGES.find((l) => l.code === code);
}

function isSupportedLanguage(code: string | null | undefined): boolean {
  return !!code && SUPPORTED_LANGUAGES.some((l) => l.code === code);
}

export function languageDirection(code: string | null | undefined): "ltr" | "rtl" {
  return languageOption(code ?? "")?.dir ?? "ltr";
}

/** `pt-BR` → `pt` when we ship `pt`; `zh-Hans-CN` → `zh`; `pa-PK` → `pa-Arab`; else null. */
export function normalizeLanguage(raw: string | null | undefined): string | null {
  return matchLanguageTag(raw, supportedLanguageCodes());
}

export function resolveDeviceLanguage(candidates: readonly string[] | undefined): string {
  for (const candidate of candidates ?? []) {
    const match = normalizeLanguage(candidate);
    if (match) {
      return match;
    }
  }
  return DEFAULT_LANGUAGE;
}

/** The device locale via Hermes' `Intl` (the OS locale on both platforms), no native module. */
export function deviceLanguageCandidates(): string[] {
  try {
    const locale = Intl.DateTimeFormat().resolvedOptions().locale;
    return locale ? [locale] : [];
  } catch {
    return [];
  }
}

/** Locale-aware uppercase: `toUpperCase()` spells Turkish wrong. */
export function localeUpperCase(value: string, language: string): string {
  return value.toLocaleUpperCase(language);
}

/** SecureStore key for the choice; keys may not contain `:`, so dots. */
export function languageKey(userId: string | null | undefined): string {
  if (!userId) {
    return "pollis-language.device";
  }
  return `pollis-language.user.${userId.replace(/[^A-Za-z0-9._-]/g, "_")}`;
}
