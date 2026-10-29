/**
 * The languages Pollis ships a catalogue for — a copy of
 * `frontend/src/i18n/languages.ts`, since mobile cannot import frontend
 * TypeScript. `tests/i18n.test.ts` fails if it drifts from the locale
 * directories. Pure (no React Native) so it runs under `node --test`.
 */

export interface LanguageOption {
  /** BCP-47 base tag, lowercase. Region subtags are stripped — see `normalizeLanguage`. */
  code: string;
  /** The language's own name for itself (endonym), never its English name. */
  label: string;
  /** Writing direction. Drives `I18nManager` — see `index.ts`. */
  dir: "ltr" | "rtl";
}

/** The language every missing key falls back to. Always present. */
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

export function isSupportedLanguage(code: string | null | undefined): boolean {
  return !!code && SUPPORTED_LANGUAGES.some((l) => l.code === code);
}

/** Writing direction for a language code; unknown codes are treated as `ltr`. */
export function languageDirection(code: string | null | undefined): "ltr" | "rtl" {
  return languageOption(code ?? "")?.dir ?? "ltr";
}

/**
 * Reduce an arbitrary BCP-47 tag to a supported language code, or null.
 * `pt-BR` → `pt` when we ship `pt`; `zh-Hans-CN` → `zh`.
 */
export function normalizeLanguage(raw: string | null | undefined): string | null {
  if (!raw) {
    return null;
  }
  const tag = raw.trim().toLowerCase();
  if (!tag) {
    return null;
  }
  if (isSupportedLanguage(tag)) {
    return tag;
  }
  const base = tag.split(/[-_]/)[0];
  if (isSupportedLanguage(base)) {
    return base;
  }
  return null;
}

/**
 * Pick a starting language from the device's locale list, most-preferred
 * first, falling back to English when we ship no catalogue for any of them.
 */
export function resolveDeviceLanguage(candidates: readonly string[] | undefined): string {
  for (const candidate of candidates ?? []) {
    const match = normalizeLanguage(candidate);
    if (match) {
      return match;
    }
  }
  return DEFAULT_LANGUAGE;
}

/**
 * The device locale. Hermes answers `Intl.DateTimeFormat().resolvedOptions()`
 * with the OS locale on both platforms, so no `expo-localization` prebuild is
 * needed.
 */
export function deviceLanguageCandidates(): string[] {
  try {
    const locale = Intl.DateTimeFormat().resolvedOptions().locale;
    return locale ? [locale] : [];
  } catch {
    return [];
  }
}

/**
 * Uppercase already-translated copy the way the language itself does; the
 * locale-invariant `toUpperCase()` spells Turkish wrong and is a no-op in
 * Arabic and Chinese.
 */
export function localeUpperCase(value: string, language: string): string {
  return value.toLocaleUpperCase(language);
}

/**
 * The SecureStore key the language choice is stored under (`storage.ts`).
 * Here rather than there so it stays importable without the native module.
 * SecureStore keys may not contain `:`, so the desktop key shape is spelled
 * with dots, and the user id is reduced to the characters it allows.
 */
export function languageKey(userId: string | null | undefined): string {
  if (!userId) {
    return "pollis-language.device";
  }
  return `pollis-language.user.${userId.replace(/[^A-Za-z0-9._-]/g, "_")}`;
}
