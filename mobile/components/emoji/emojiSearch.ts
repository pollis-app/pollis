/**
 * Search, display, and recency logic for the emoji picker.
 *
 * Ported from frontend/src/components/Emoji/emojiSearch.ts. Pure functions;
 * the recents/tone storage lives in lib/emojiPrefs.ts (file-backed — RN has
 * no localStorage).
 */

import type { CustomEmoji } from "../../hooks/queries/useEmoji";
import { applySkinTone, STANDARD_EMOJI, type StandardEmoji } from "./emojiData";
import { emojiTokenText } from "./emojiTokens";

/** A picker cell: either a Unicode emoji or a custom per-group one. */
export type PickerEmoji =
  | { kind: "standard"; emoji: StandardEmoji }
  | { kind: "custom"; emoji: CustomEmoji };

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
 * `emojiData.ts` stores bare codepoints with no variation selector. The
 * selector is added here, at render time: never when a skin tone was applied
 * (a Fitzpatrick modifier already implies emoji presentation), and only for
 * single-codepoint bases below U+1F000 (the legacy BMP pictographs that
 * default to text presentation).
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

/**
 * What a chosen cell turns into: a standard emoji becomes the displayed
 * character (skin tone + variation selector baked in); a custom emoji becomes
 * its `<:shortcode:hash>` wire token.
 */
export function pickerEmojiInsertText(
  item: PickerEmoji,
  toneIndex: number,
): string {
  if (item.kind === "standard") {
    return emojiDisplayChar(item.emoji, toneIndex);
  }
  return emojiTokenText(item.emoji.shortcode, item.emoji.content_hash);
}

/**
 * Rank `query` against the emoji set. Three tiers — exact, prefix, substring —
 * shorter haystack wins within a tier, and custom emoji outrank standard ones
 * at equal tier.
 */
export function searchEmoji(
  query: string,
  custom: readonly CustomEmoji[],
): PickerEmoji[] {
  const needle = query.trim().toLowerCase();
  if (!needle) {
    return [];
  }

  const scored: { item: PickerEmoji; tier: number; length: number }[] = [];

  const consider = (item: PickerEmoji, haystack: string, bonus: number) => {
    const index = haystack.indexOf(needle);
    if (index < 0) {
      return;
    }
    let tier: number;
    if (haystack === needle) {
      tier = 0;
    } else if (index === 0) {
      tier = 1;
    } else {
      tier = 2;
    }
    scored.push({ item, tier: tier * 2 + bonus, length: haystack.length });
  };

  for (const emoji of custom) {
    consider({ kind: "custom", emoji }, emoji.shortcode.toLowerCase(), 0);
  }
  for (const emoji of STANDARD_EMOJI) {
    consider({ kind: "standard", emoji }, emoji.name, 1);
  }

  scored.sort((a, b) => a.tier - b.tier || a.length - b.length);
  return scored.map((s) => s.item);
}

/**
 * Resolve recents ids back to live emoji. Ids that no longer resolve are
 * dropped silently (deleted custom emoji, lost group access).
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
