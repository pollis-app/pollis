/**
 * The standard half of the composer's `:shortcode:` index: the ONLY shortcode
 * module that imports `emojiData.ts`, loaded via dynamic import on the first
 * typed colon so the table stays out of the startup chunk (#874).
 */

import { STANDARD_EMOJI } from "./emojiData";
import { emojiDisplayChar, readSkinTone } from "./emojiSearch";
import { standardEntry, type ShortcodeEntry } from "./emojiShortcodeQuery";

/**
 * One entry per (alias, emoji) pair. A function, not a constant, because it
 * reads the current skin-tone preference.
 */
export function standardShortcodeEntries(): ShortcodeEntry[] {
  const toneIndex = readSkinTone();
  const entries: ShortcodeEntry[] = [];
  for (const emoji of STANDARD_EMOJI) {
    if (emoji.shortcodes.length === 0) {
      continue;
    }
    const displayChar = emojiDisplayChar(emoji, toneIndex);
    for (const shortcode of emoji.shortcodes) {
      entries.push(standardEntry(emoji, shortcode, displayChar));
    }
  }
  return entries;
}
