// Pure search/display/recency logic for the emoji picker (a port of desktop's
// emojiSearch.ts); recents/tone storage lives in lib/emojiPrefs.ts.

import type { CustomEmoji } from "../../hooks/queries/useEmoji";
import { applySkinTone, STANDARD_EMOJI, type StandardEmoji } from "./emojiData";
import { NO_ANNOTATIONS, type EmojiAnnotationStack } from "./emojiAnnotations";
import { rankEmoji, type PickerEmoji } from "./emojiRank";
import { emojiTokenText } from "./emojiTokens";

export type { PickerEmoji } from "./emojiRank";

/** Stable identity for a cell — the recents key and the React key. */
export function pickerEmojiId(item: PickerEmoji): string {
  if (item.kind === "standard") {
    return `s:${item.emoji.char}`;
  }
  return `c:${item.emoji.content_hash}`;
}

/**
 * The character to display (and insert) for a standard emoji. `emojiData.ts`
 * stores bare codepoints; the variation selector is added here, never after a
 * skin tone (the modifier implies emoji presentation), and only for
 * single-codepoint bases below U+1F000, which default to text presentation.
 */
export function emojiDisplayChar(
  emoji: StandardEmoji,
  toneIndex: number,
): string {
  const toned = applySkinTone(emoji, toneIndex);
  if (toned !== emoji.char) {
    return toned;
  }
  const codepoints = Array.from(emoji.char);
  if (codepoints.length !== 1) {
    return emoji.char;
  }
  const cp = emoji.char.codePointAt(0) ?? 0;
  if (cp >= 0x1f000) {
    return emoji.char;
  }
  return `${emoji.char}️`;
}

/** A standard emoji becomes its displayed character; a custom one its `<:shortcode:hash>` wire token. */
export function pickerEmojiInsertText(
  item: PickerEmoji,
  toneIndex: number,
): string {
  if (item.kind === "standard") {
    return emojiDisplayChar(item.emoji, toneIndex);
  }
  return emojiTokenText(item.emoji.shortcode, item.emoji.content_hash);
}

/** `annotations` is the active locale's table then English's; without it only Unicode names are searched. */
export function searchEmoji(
  query: string,
  custom: readonly CustomEmoji[],
  annotations: EmojiAnnotationStack = NO_ANNOTATIONS,
): PickerEmoji[] {
  return rankEmoji(query, STANDARD_EMOJI, custom, annotations);
}

/** Ids that no longer resolve (deleted custom emoji, lost group access) are dropped silently. */
export function resolveRecents(
  ids: readonly string[],
  custom: readonly CustomEmoji[],
): PickerEmoji[] {
  const standardByChar = new Map(STANDARD_EMOJI.map((e) => [e.char, e]));
  const customByHash = new Map(custom.map((e) => [e.content_hash, e]));

  const out: PickerEmoji[] = [];
  for (const id of ids) {
    if (id.startsWith("s:")) {
      const emoji = standardByChar.get(id.slice(2));
      if (emoji) {
        out.push({ kind: "standard", emoji });
      }
      continue;
    }
    if (id.startsWith("c:")) {
      const emoji = customByHash.get(id.slice(2));
      if (emoji) {
        out.push({ kind: "custom", emoji });
      }
    }
  }
  return out;
}
