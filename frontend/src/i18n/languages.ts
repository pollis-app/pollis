/**
 * The languages Pollis ships a catalogue for: the only place a language code
 * is listed (see `frontend/src/i18n/README.md`).
 */

export interface LanguageOption {
  /** BCP-47 base tag, lowercase; regions are stripped (`normalizeLanguage`). */
  code: string;
  /** The endonym, NOT the English name: the reader may not read English. */
  label: string;
  dir: "ltr" | "rtl";
}

export const DEFAULT_LANGUAGE = "en";

/** The shipped languages. **Edit this array to add a locale.** */
const SHIPPED_LANGUAGES: readonly LanguageOption[] = [
  { code: "en", label: "English", dir: "ltr" },
  { code: "es", label: "Español", dir: "ltr" },
  { code: "uk", label: "Українська", dir: "ltr" },
  { code: "fr", label: "Français", dir: "ltr" },
  { code: "ru", label: "Русский", dir: "ltr" },
  // `zh`, not `zh-Hans`: no casing of the script subtag passes both
  // `normalizeLanguage` and i18next. See `.codesight/wiki/i18n.md`.
  { code: "zh", label: "简体中文", dir: "ltr" },
  { code: "ar", label: "العربية", dir: "rtl" },
];

/** Seeded from `SHIPPED_LANGUAGES`; under Playwright, extendable (`registerTestLanguage`). */
const registry: LanguageOption[] = [...SHIPPED_LANGUAGES];

export function supportedLanguages(): readonly LanguageOption[] {
  return registry;
}

export function supportedLanguageCodes(): string[] {
  return registry.map((l) => l.code);
}

export function languageOption(code: string): LanguageOption | undefined {
  return registry.find((l) => l.code === code);
}

export function isSupportedLanguage(code: string | null | undefined): boolean {
  return !!code && registry.some((l) => l.code === code);
}

/**
 * Playwright-only: add a synthetic locale so e2e can prove switching,
 * persistence and English fallback without depending on shipped copy.
 * Eliminated from the real bundle (`VITE_PLAYWRIGHT`). Call BEFORE `i18n.init`,
 * which snapshots `supportedLngs`.
 */
export function registerTestLanguage(option: LanguageOption): void {
  if (import.meta.env.VITE_PLAYWRIGHT !== "true") {
    return;
  }
  if (registry.some((l) => l.code === option.code)) {
    return;
  }
  registry.push(option);
}

export function languageDirection(code: string | null | undefined): "ltr" | "rtl" {
  return languageOption(code ?? "")?.dir ?? "ltr";
}

/**
 * Uppercase translated copy in `language`'s own rules (Turkish i → İ), not the
 * invariant mapping. The only sanctioned way to case translated text (#911).
 */
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

/** First shipped language in the OS/browser preference list, else English. */
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
