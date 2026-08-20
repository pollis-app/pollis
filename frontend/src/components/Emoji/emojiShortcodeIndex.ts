/**
 * The standard half of the composer's `:shortcode:` index.
 *
 * This is the ONLY module in the shortcode path that imports `emojiData.ts`,
 * and it exists so that stays true: the table is the largest module in the
 * bundle and #874 deliberately code-split it out of the startup chunk behind
 * the picker's lazy import. The composer is always mounted, so it reaches this
 * through `loadStandardShortcodeEntries()` — a dynamic import fired the first
 * time someone actually types a colon — rather than a static one that would put
 * ~96 KB back into the first paint.
 */

import { STANDARD_EMOJI } from "./emojiData";
import { emojiDisplayChar, readSkinTone } from "./emojiSearch";
import { standardEntry, type ShortcodeEntry } from "./emojiShortcodeQuery";

/**
 * One entry per (alias, emoji) pair, so `:+1` and `:thumbsup` are separately
 * matchable rows that happen to insert the same character.
 *
 * Built against the CURRENT skin-tone preference, which is why this is a
 * function rather than a module constant: a tone chosen in the picker must
 * apply to the next emoji completed in the composer too.
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
