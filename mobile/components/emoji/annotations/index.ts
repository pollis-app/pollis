// GENERATED FILE — DO NOT EDIT BY HAND.
//
// Produced by `scripts/generate-emoji-data.py` from the vendored CLDR emoji
// annotations in scripts/emoji-annotations.json (CLDR release-48). To refresh
// the data, or after adding a locale to i18n/languages.ts:
//
//     python3 scripts/generate-emoji-data.py --refresh-annotations
//
// English is a module in the bundle: the instant fallback. Every other locale is a
// content-addressed asset, `<locale>.emojitable` (JSON rows), read when the picker
// first needs it (#2188): Metro does not split, so a module per locale put every
// table in the Hermes bundle and every OTA re-sent them all. A read that fails or
// stalls rejects (`readEmojiTable` bounds it), which leaves English results only.

import { readEmojiTable } from "../emojiTableAsset.ts";

/** `[char, localized name, `|`-joined lowercase keywords]`. */
export type EmojiAnnotationRow = readonly [
  char: string,
  name: string,
  keywords: string,
];

export const EMOJI_ANNOTATION_LOCALES: readonly string[] = [
  "am",
  "ar",
  "bn",
  "en",
  "es",
  "fa",
  "fr",
  "ha",
  "hi",
  "ja",
  "ko",
  "mr",
  "pa-Arab",
  "pa-Guru",
  "ps",
  "pt",
  "ru",
  "sw",
  "ta",
  "te",
  "uk",
  "ur",
  "yo",
  "zh",
  "zu",
];

/** The rows for `locale`, or null when no table ships for it. Never throws. */
export function loadEmojiAnnotationRows(
  locale: string,
): Promise<readonly EmojiAnnotationRow[]> | null {
  switch (locale) {
    case "am":
      return readEmojiTable(() => require("./am.emojitable"));
    case "ar":
      return readEmojiTable(() => require("./ar.emojitable"));
    case "bn":
      return readEmojiTable(() => require("./bn.emojitable"));
    case "en":
      return import("./en").then((m) => m.default);
    case "es":
      return readEmojiTable(() => require("./es.emojitable"));
    case "fa":
      return readEmojiTable(() => require("./fa.emojitable"));
    case "fr":
      return readEmojiTable(() => require("./fr.emojitable"));
    case "ha":
      return readEmojiTable(() => require("./ha.emojitable"));
    case "hi":
      return readEmojiTable(() => require("./hi.emojitable"));
    case "ja":
      return readEmojiTable(() => require("./ja.emojitable"));
    case "ko":
      return readEmojiTable(() => require("./ko.emojitable"));
    case "mr":
      return readEmojiTable(() => require("./mr.emojitable"));
    case "pa-Arab":
      return readEmojiTable(() => require("./pa-Arab.emojitable"));
    case "pa-Guru":
      return readEmojiTable(() => require("./pa-Guru.emojitable"));
    case "ps":
      return readEmojiTable(() => require("./ps.emojitable"));
    case "pt":
      return readEmojiTable(() => require("./pt.emojitable"));
    case "ru":
      return readEmojiTable(() => require("./ru.emojitable"));
    case "sw":
      return readEmojiTable(() => require("./sw.emojitable"));
    case "ta":
      return readEmojiTable(() => require("./ta.emojitable"));
    case "te":
      return readEmojiTable(() => require("./te.emojitable"));
    case "uk":
      return readEmojiTable(() => require("./uk.emojitable"));
    case "ur":
      return readEmojiTable(() => require("./ur.emojitable"));
    case "yo":
      return readEmojiTable(() => require("./yo.emojitable"));
    case "zh":
      return readEmojiTable(() => require("./zh.emojitable"));
    case "zu":
      return readEmojiTable(() => require("./zu.emojitable"));
    default:
      return null;
  }
}
