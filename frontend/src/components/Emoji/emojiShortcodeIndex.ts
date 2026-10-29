/** The only shortcode-path module importing `emojiData.ts`; reached by dynamic import (#874). */

import { STANDARD_EMOJI } from "./emojiData";
import { emojiDisplayChar, readSkinTone } from "./emojiSearch";
import { standardEntry, type ShortcodeEntry } from "./emojiShortcodeQuery";

/** One entry per (alias, emoji); a function so the current skin tone applies. */
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
