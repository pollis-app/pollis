/**
 * The set of languages Pollis ships a catalogue for.
 *
 * Adding a locale is a two-step change and this list is step two — see
 * `frontend/src/i18n/README.md`. Nothing else in the app hardcodes a language
 * code, so the selector, the OS-locale probe and the i18next `supportedLngs`
 * all widen together from this one array.
 */

export interface LanguageOption {
  /** BCP-47 base tag, lowercase. Region subtags are stripped — see `normalizeLanguage`. */
  code: string;
  /**
   * The language's own name for itself (endonym), NOT its English name.
   * A language picker that says "German" is useless to someone who only
   * reads German, which is exactly the person reaching for it.
   */
  label: string;
  /** Writing direction. Latin/Cyrillic scripts are `ltr`; Arabic/Hebrew are `rtl`. */
  dir: "ltr" | "rtl";
}

/** The language every missing key falls back to. Always present. */
export const DEFAULT_LANGUAGE = "en";

/**
 * The languages Pollis ships. **This is the array you edit to add a locale.**
 */
const SHIPPED_LANGUAGES: readonly LanguageOption[] = [
  { code: "en", label: "English", dir: "ltr" },
  { code: "es", label: "Español", dir: "ltr" },
  { code: "uk", label: "Українська", dir: "ltr" },
  { code: "fr", label: "Français", dir: "ltr" },
  { code: "ru", label: "Русский", dir: "ltr" },
  // Simplified Chinese ships as the base tag `zh`, not `zh-Hans`. The script
  // subtag cannot survive this file's own contract: `normalizeLanguage`
  // lowercases every tag, while i18next canonicalizes codes through
  // `Intl.getCanonicalLocales` before testing them against `supportedLngs`.
  // A `zh-hans` entry is therefore rejected by i18next and a `zh-Hans` entry
  // is rejected by `normalizeLanguage` — either way the catalogue silently
  // renders English. `zh` is also what every real OS tag degrades to
  // (`zh-CN`, `zh-Hans-CN`), which the script subtag would not have matched.
  { code: "zh", label: "简体中文", dir: "ltr" },
  { code: "ar", label: "العربية", dir: "rtl" },
];

/**
 * The live registry. Seeded from `SHIPPED_LANGUAGES` and, under Playwright
 * only, extendable with a synthetic locale — see `registerTestLanguage`.
 */
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

/**
 * Playwright-only: add a synthetic locale so the e2e suite can prove that
 * switching language changes rendered copy, that the choice persists, and
 * that a key missing from a catalogue falls back to English — none of which
 * is assertable against the shipped catalogues alone: they are complete, so
 * there is no missing key to fall back on, and asserting on real translated
 * copy would tie the suite to whichever locales happen to ship.
 *
 * Guarded by `VITE_PLAYWRIGHT`, so it is dead-code-eliminated from the real
 * bundle. Same mechanism as `__pollisStore` / `__pollisQueryClient` in
 * `main.tsx`, and for the same reason: some behaviour can only be proven
 * from outside the React tree.
 *
 * Must be called BEFORE `i18n.init`, because `supportedLngs` is snapshotted
 * there and `changeLanguage` rejects anything outside it.
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

/** Writing direction for a language code; unknown codes are treated as `ltr`. */
export function languageDirection(code: string | null | undefined): "ltr" | "rtl" {
  return languageOption(code ?? "")?.dir ?? "ltr";
}

/**
 * Reduce an arbitrary BCP-47 tag to a supported language code, or null.
 *
 * `pt-BR` → `pt` when we ship `pt`, because a regional catalogue we do not
 * have should still land on the language the user actually reads rather than
 * dropping all the way to English.
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
 * Pick a starting language from the OS/browser locale list, in preference
 * order, falling back to English when we ship no catalogue for any of them.
 *
 * Under Tauri the webview inherits the OS locale, so `navigator.languages` is
 * the OS's answer; under the Playwright browser tier it is whatever the test
 * sets. One code path serves both.
 */
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
