/** Search and recency for the emoji picker: pure functions plus a `localStorage` recents list. */

import type { CustomEmoji } from "../../types";
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
 * stores bare codepoints (a baked-in U+FE0F would break `applySkinTone`), so
 * text-default BMP pictographs (☝ ⌨ ☎ ❤) get U+FE0F here, but never with a
 * skin tone (non-canonical) and never on multi-codepoint flags.
 */
export function emojiDisplayChar(emoji: StandardEmoji, toneIndex: number): string {
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

/**
 * Inserted on pick: the displayed character (tone and selector included), or a
 * custom emoji's wire token, which every recipient resolves to an image.
 */
export function pickerEmojiInsertText(item: PickerEmoji, toneIndex: number): string {
  if (item.kind === "standard") {
    return emojiDisplayChar(item.emoji, toneIndex);
  }
  return emojiTokenText(item.emoji.shortcode, item.emoji.content_hash);
}

/**
 * Rank `query` against the standard table plus `custom`. `annotations` is the
 * active locale's table then English's; without it only Unicode names match.
 */
export function searchEmoji(
  query: string,
  custom: readonly CustomEmoji[],
  annotations: EmojiAnnotationStack = NO_ANNOTATIONS,
): PickerEmoji[] {
  return rankEmoji(query, STANDARD_EMOJI, custom, annotations);
}

const RECENTS_KEY = "pollis.emoji.recents";
const RECENTS_MAX = 40;

/** Read the recents list. A corrupt or foreign value yields []: a broken preference must never stop the picker. */
export function readRecentEmojiIds(): string[] {
  try {
    const raw = window.localStorage.getItem(RECENTS_KEY);
    if (!raw) {
      return [];
    }
    const parsed: unknown = JSON.parse(raw);
    if (!Array.isArray(parsed)) {
      return [];
    }
    return parsed.filter((v): v is string => typeof v === "string").slice(0, RECENTS_MAX);
  } catch {
    return [];
  }
}

/** Push `id` to the front of the recents list, de-duplicated and capped. */
export function recordRecentEmojiId(id: string): string[] {
  const next = [id, ...readRecentEmojiIds().filter((v) => v !== id)].slice(0, RECENTS_MAX);
  try {
    window.localStorage.setItem(RECENTS_KEY, JSON.stringify(next));
  } catch {
    // A full or disabled localStorage only costs the recents list.
  }
  return next;
}

/**
 * Resolve recents ids back to live emoji. Unresolvable ids (deleted emoji, lost
 * group access) are dropped silently rather than rendered as broken cells.
 */
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

const TONE_KEY = "pollis.emoji.tone";

export function readSkinTone(): number {
  try {
    const raw = window.localStorage.getItem(TONE_KEY);
    const parsed = raw === null ? 0 : Number.parseInt(raw, 10);
    if (!Number.isFinite(parsed) || parsed < 0 || parsed > 5) {
      return 0;
    }
    return parsed;
  } catch {
    return 0;
  }
}

export function writeSkinTone(toneIndex: number): void {
  try {
    window.localStorage.setItem(TONE_KEY, String(toneIndex));
  } catch {
    // Same as recents: a lost preference is not worth an error path.
  }
}
