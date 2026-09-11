/**
 * Search and recency for the emoji picker.
 *
 * Pure functions plus a small `localStorage`-backed recents list — no React, so
 * the ranking is testable and the picker component stays about layout.
 */

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
 * The character to actually display (and insert) for a standard emoji.
 *
 * `emojiData.ts` stores bare codepoints with no variation selector, on purpose:
 * baking U+FE0F in would make the tonable bases multi-codepoint and break
 * `applySkinTone`'s "insert after the first codepoint" contract. But ~190
 * entries — the legacy BMP pictographs like ☝ ⌨ ☎ ❤ — default to *text*
 * presentation, so without a selector they render as monochrome glyphs rather
 * than emoji.
 *
 * So the selector is added here, at render time, under two conditions:
 *
 *   - Never when a skin tone was applied. A Fitzpatrick modifier already implies
 *     emoji presentation, and `<base> FE0F <modifier>` is not the canonical
 *     sequence.
 *   - Only for SINGLE-codepoint bases below U+1F000. Astral pictographs
 *     (U+1F300+) already default to emoji presentation, and multi-codepoint
 *     entries are the flags — regional-indicator pairs, which must not be
 *     touched.
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
 * What gets inserted into the composer when a cell is chosen.
 *
 * A standard emoji inserts the displayed character (skin tone and variation
 * selector included, so the recipient sees what the sender picked); a custom
 * emoji inserts its wire token, which is what travels inside the E2EE message
 * body and what every recipient — member of the owning group or not — resolves
 * to an image.
 */
export function pickerEmojiInsertText(item: PickerEmoji, toneIndex: number): string {
  if (item.kind === "standard") {
    return emojiDisplayChar(item.emoji, toneIndex);
  }
  return emojiTokenText(item.emoji.shortcode, item.emoji.content_hash);
}

/**
 * Rank `query` against the whole standard table plus `custom`.
 *
 * `annotations` is the active locale's table followed by English's (from
 * `useEmojiAnnotations`); without it only the Unicode names are searched. The
 * ranking itself lives in `emojiRank.ts`, where it is unit-tested.
 */
export function searchEmoji(
  query: string,
  custom: readonly CustomEmoji[],
  annotations: EmojiAnnotationStack = NO_ANNOTATIONS,
): PickerEmoji[] {
  return rankEmoji(query, STANDARD_EMOJI, custom, annotations);
}

// ── Recently used ───────────────────────────────────────────────────────────

const RECENTS_KEY = "pollis.emoji.recents";
const RECENTS_MAX = 40;

/**
 * Read the recents list.
 *
 * Deliberately tolerant: a corrupt or foreign value yields an empty list rather
 * than throwing, because a broken preference must never be able to stop the
 * picker from opening.
 */
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
    // A full or disabled localStorage costs the user their recents list and
    // nothing else — never the picker.
  }
  return next;
}

/**
 * Resolve recents ids back to live emoji.
 *
 * Ids that no longer resolve are dropped silently: a custom emoji can be
 * deleted, or the user can lose access to the group that owned it, and either
 * way it must disappear from the picker rather than render as a broken cell.
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

// ── Skin tone preference ────────────────────────────────────────────────────

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
