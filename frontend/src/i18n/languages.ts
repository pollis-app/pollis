/** A shipped language; this registry drives the selector, OS probe and `supportedLngs` (see i18n/README.md). */

export interface LanguageOption {
  /** BCP-47 base tag, lowercase. Region subtags are stripped — see `normalizeLanguage`. */
  code: string;
  /** The language's own name for itself (endonym). */
  label: string;
  dir: "ltr" | "rtl";
}

/** The language every missing key falls back to. Always present. */
export const DEFAULT_LANGUAGE = "en";

/** The languages Pollis ships; edit this to add a locale. */
const SHIPPED_LANGUAGES: readonly LanguageOption[] = [
  { code: "en", label: "English", dir: "ltr" },
  { code: "es", label: "Español", dir: "ltr" },
  { code: "uk", label: "Українська", dir: "ltr" },
  { code: "fr", label: "Français", dir: "ltr" },
  { code: "ru", label: "Русский", dir: "ltr" },
  // `zh`, not `zh-Hans`: i18next and `normalizeLanguage` disagree on casing a script subtag.
  { code: "zh", label: "简体中文", dir: "ltr" },
  { code: "ar", label: "العربية", dir: "rtl" },
];

/** The live registry; Playwright may add a synthetic locale (`registerTestLanguage`). */
const registry: LanguageOption[] = [...SHIPPED_LANGUAGES];

/** Every selectable language, in selector order. */
export function supportedLanguages(): readonly LanguageOption[] {
  return registry;
}

/** Every selectable language code, in selector order. */
export function supportedLanguageCodes(): string[] {
  return registry.map((l) => l.code);
}

export function languageOption(code: string): LanguageOption | undefined {
  return registry.find((l) => l.code === code);
}

export function isSupportedLanguage(code: string | null | undefined): boolean {
  return !!code && registry.some((l) => l.code === code);
}

/** Playwright-only synthetic locale, for fallback and persistence specs. Call before `i18n.init`. */
export function registerTestLanguage(option: LanguageOption): void {
  if (import.meta.env.VITE_PLAYWRIGHT !== "true") {
    return;
  }
  if (registry.some((l) => l.code === option.code)) {
    return;
  }
  registry.push(option);
}

/** Writing direction for a language code; unknown codes are treated as `ltr`. */
export function languageDirection(code: string | null | undefined): "ltr" | "rtl" {
  return languageOption(code ?? "")?.dir ?? "ltr";
}

/** Uppercase translated copy in `language`'s own rules (Turkish i/İ); the only sanctioned way (#911). */
export function localeUpperCase(value: string, language: string): string {
  return value.toLocaleUpperCase(language);
}

/** Reduce a BCP-47 tag to a supported code (`pt-BR` → `pt`), or null. */
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

/** First supported language from the OS/browser list, else English. */
export function resolveOsLanguage(
  candidates: readonly string[] | undefined,
): string {
  for (const candidate of candidates ?? []) {
    const match = normalizeLanguage(candidate);
    if (match) {
      return match;
    }
  }
  return DEFAULT_LANGUAGE;
}

/** The browser/OS locale preference list, most-preferred first. */
export function osLanguageCandidates(): string[] {
  try {
    const list = navigator.languages;
    if (list && list.length > 0) {
      return [...list];
    }
    return navigator.language ? [navigator.language] : [];
  } catch {
    return [];
  }
}
