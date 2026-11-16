// GENERATED FILE — DO NOT EDIT BY HAND.
//
// Produced by `scripts/generate-emoji-data.py` from the vendored CLDR emoji
// annotations in scripts/emoji-annotations.json (CLDR release-48). To refresh
// the data, or after adding a locale to i18n/languages.ts:
//
//     python3 scripts/generate-emoji-data.py --refresh-annotations
//
// Each locale is its own module behind a dynamic import. In the web bundle that
// is a separate chunk (the picker needs the active locale's table and English's,
// never all of them); on native, where Metro does not split, every locale ships
// in the binary and the import only defers parsing until the picker opens.

/** `[char, localized name, `|`-joined lowercase keywords]`. */
export type EmojiAnnotationRow = readonly [
  char: string,
  name: string,
  keywords: string,
];

export const EMOJI_ANNOTATION_LOCALES: readonly string[] = [
  "ar",
  "bn",
  "en",
  "es",
  "fr",
  "hi",
  "ja",
  "ko",
  "mr",
  "pa-Arab",
  "pa-Guru",
  "ps",
  "ru",
  "sw",
  "ta",
  "te",
  "uk",
  "ur",
  "zh",
];

/** The rows for `locale`, or null when no table ships for it. */
export function loadEmojiAnnotationRows(
  locale: string,
): Promise<readonly EmojiAnnotationRow[]> | null {
  switch (locale) {
    case "ar":
      return import("./ar").then((m) => m.default);
    case "bn":
      return import("./bn").then((m) => m.default);
    case "en":
      return import("./en").then((m) => m.default);
    case "es":
      return import("./es").then((m) => m.default);
    case "fr":
      return import("./fr").then((m) => m.default);
    case "hi":
      return import("./hi").then((m) => m.default);
    case "ja":
      return import("./ja").then((m) => m.default);
    case "ko":
      return import("./ko").then((m) => m.default);
    case "mr":
      return import("./mr").then((m) => m.default);
    case "pa-Arab":
      return import("./pa-Arab").then((m) => m.default);
    case "pa-Guru":
      return import("./pa-Guru").then((m) => m.default);
    case "ps":
      return import("./ps").then((m) => m.default);
    case "ru":
      return import("./ru").then((m) => m.default);
    case "sw":
      return import("./sw").then((m) => m.default);
    case "ta":
      return import("./ta").then((m) => m.default);
    case "te":
      return import("./te").then((m) => m.default);
    case "uk":
      return import("./uk").then((m) => m.default);
    case "ur":
      return import("./ur").then((m) => m.default);
    case "zh":
      return import("./zh").then((m) => m.default);
    default:
      return null;
  }
}
