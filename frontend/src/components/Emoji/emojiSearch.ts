/** Emoji picker search and recents; no React. */

import type { CustomEmoji } from "../../types";
import { applySkinTone, STANDARD_EMOJI, type StandardEmoji } from "./emojiData";
import { NO_ANNOTATIONS, type EmojiAnnotationStack } from "./emojiAnnotations";
import { rankEmoji, type PickerEmoji } from "./emojiRank";
import { emojiTokenText } from "./emojiTokens";

export type { PickerEmoji } from "./emojiRank";

export function pickerEmojiId(item: PickerEmoji): string {
  if (item.kind === "standard") {
    return `s:${item.emoji.char}`;
  }
  return `c:${item.emoji.content_hash}`;
}

/** Adds U+FE0F for single-codepoint bases below U+1F000 (text-presentation by default), never with a skin tone or on flags. */
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

/** Standard: the displayed character; custom: its wire token. */
export function pickerEmojiInsertText(item: PickerEmoji, toneIndex: number): string {
  if (item.kind === "standard") {
    return emojiDisplayChar(item.emoji, toneIndex);
  }
  return emojiTokenText(item.emoji.shortcode, item.emoji.content_hash);
}

/** Ranking lives in `emojiRank.ts`; `annotations` adds localized names. */
export function searchEmoji(
  query: string,
  custom: readonly CustomEmoji[],
  annotations: EmojiAnnotationStack = NO_ANNOTATIONS,
): PickerEmoji[] {
  return rankEmoji(query, STANDARD_EMOJI, custom, annotations);
}

const RECENTS_KEY = "pollis.emoji.recents";
const RECENTS_MAX = 40;

/** Tolerant: a corrupt value yields an empty list, never a broken picker. */
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

export function recordRecentEmojiId(id: string): string[] {
  const next = [id, ...readRecentEmojiIds().filter((v) => v !== id)].slice(0, RECENTS_MAX);
  try {
    window.localStorage.setItem(RECENTS_KEY, JSON.stringify(next));
  } catch {
    // Full or disabled localStorage: lose recents, nothing else.
  }
  return next;
}

/** Unresolvable ids (deleted, access lost) are dropped silently. */
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
    // A lost preference isn't worth an error path.
  }
}
