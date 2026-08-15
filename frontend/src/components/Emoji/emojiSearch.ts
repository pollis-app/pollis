/**
 * Search and recency for the emoji picker.
 *
 * Pure functions plus a small `localStorage`-backed recents list — no React, so
 * the ranking is testable and the picker component stays about layout.
 */

import type { CustomEmoji } from "../../types";
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
 * What gets inserted into the composer when a cell is chosen.
 *
 * A standard emoji inserts the character (with the user's skin tone applied
 * where the base supports one); a custom emoji inserts its wire token, which is
 * what travels inside the E2EE message body and what every recipient — member
 * of the owning group or not — resolves to an image.
 */
export function pickerEmojiInsertText(item: PickerEmoji, toneIndex: number): string {
  if (item.kind === "standard") {
    return applySkinTone(item.emoji, toneIndex);
  }
  return emojiTokenText(item.emoji.shortcode, item.emoji.content_hash);
}

/**
 * Rank `query` against the emoji set.
 *
 * Three tiers, in this order, because that is the order a person expects:
 * an exact name/shortcode match, then a prefix match, then a substring match.
 * Within a tier the shorter name wins — searching "cat" should surface "cat"
 * above "cat with wry smile".
 *
 * Custom emoji rank ahead of standard ones at equal tier: someone who typed
 * three letters in a server full of custom emoji is far more likely to mean one
 * of those.
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
