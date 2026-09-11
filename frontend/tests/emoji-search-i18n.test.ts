/*
 * Localized emoji search (#901).
 *
 * The picker's search used to match the English Unicode name and nothing else,
 * so a Spanish user got a translated picker where `corazón` found nothing.
 * These tests drive the REAL generated tables — `emojiData.ts` plus the CLDR
 * annotation modules under `annotations/` — through the pure ranking, so a
 * regeneration that dropped a locale, an entry, or the English fallback fails
 * here rather than silently in the UI.
 *
 * Ukrainian is the locale asserted on because it is a four-form-plural
 * language (one / few / many / other), which is where a naive keying scheme
 * would have broken first.
 *
 *   node --test frontend/tests/
 */

import test from "node:test";
import assert from "node:assert/strict";

import { STANDARD_EMOJI } from "../src/components/Emoji/emojiData.ts";
import {
  buildEmojiAnnotations,
  emojiDisplayName,
  type EmojiAnnotations,
} from "../src/components/Emoji/emojiAnnotations.ts";
import { rankEmoji, type PickerEmoji } from "../src/components/Emoji/emojiRank.ts";
import { EMOJI_ANNOTATION_LOCALES } from "../src/components/Emoji/annotations/index.ts";
import { supportedLanguageCodes } from "../src/i18n/languages.ts";
import type { CustomEmoji } from "../src/types/index.ts";

async function loadLocale(locale: string): Promise<EmojiAnnotations> {
  const module = await import(`../src/components/Emoji/annotations/${locale}.ts`);
  return buildEmojiAnnotations(module.default);
}

const [en, uk, es, fr] = await Promise.all(["en", "uk", "es", "fr"].map(loadLocale));

function chars(items: PickerEmoji[]): string[] {
  return items.map((item) => (item.kind === "standard" ? item.emoji.char : ""));
}

function search(query: string, tables: EmojiAnnotations[]): string[] {
  return chars(rankEmoji(query, STANDARD_EMOJI, [], tables));
}

test("a Ukrainian query finds the emoji CLDR names in Ukrainian", () => {
  // "серце" — heart. 🫀 is NAMED exactly that in Ukrainian, so it ranks first;
  // ❤ ("червоне серце") and the other hearts follow by keyword.
  const hits = search("серце", [uk, en]);
  assert.equal(hits[0], "🫀");
  assert.ok(hits.includes("❤"));
  assert.ok(hits.includes("💛"));
});

test("English keeps working for a Ukrainian user", () => {
  const hits = search("heart", [uk, en]);
  assert.ok(hits.includes("❤"));
  assert.ok(hits.includes("💔"));
});

test("a Spanish query with an accent finds the emoji", () => {
  assert.equal(search("corazón", [es, en])[0], "❤");
  assert.equal(search("cara llorando de risa", [es, en])[0], "😂");
});

test("an unaccented query matches the accented name — phones rarely have dead keys", () => {
  assert.equal(search("corazon", [es, en])[0], "❤");
  assert.equal(search("corazon rojo", [es, en])[0], "❤");
  assert.ok(search("cafe", [fr, en]).includes("☕"));
  assert.ok(search("patisserie", [fr, en]).length > 0);
});

test("a custom substring hit outranks every standard keyword hit", () => {
  const custom: CustomEmoji = {
    group_id: "g",
    group_name: "g",
    shortcode: "big_heart",
    content_hash: "h",
    content_type: "image/webp",
    animated: false,
    size_bytes: 1,
    created_by: "u",
  };
  const items = rankEmoji("heart", STANDARD_EMOJI, [custom], [en]);
  const ids = items.map((item) => (item.kind === "custom" ? "custom" : item.emoji.char));
  const customAt = ids.indexOf("custom");
  // 💘 "heart with arrow" is NAMED with the word, so it may still lead; 💋
  // "kiss mark" and 💌 "love letter" only carry "heart" as a keyword and must
  // sit below the custom emoji.
  assert.ok(customAt >= 0);
  assert.ok(customAt < ids.indexOf("💋"));
  assert.ok(customAt < ids.indexOf("💌"));
  assert.ok(ids.includes("❤"));
});

test("without any table the Unicode name still ranks an exact match first", () => {
  const hits = search("grinning face", []);
  assert.equal(hits[0], "😀");
  assert.equal(search("corazón", []).length, 0);
});

test("a name outranks a keyword, and a keyword outranks a substring", () => {
  const hits = search("cat", [en]);
  // 🐈 is named "cat"; 🐱 "cat face" is a name prefix; the cat faces (😹 …)
  // carry "cat" only as a keyword.
  assert.equal(hits[0], "🐈");
  assert.equal(hits[1], "🐱");
  assert.ok(hits.indexOf("😹") > 1);
});

test("custom emoji rank ahead of standard ones at equal tier", () => {
  const custom: CustomEmoji = {
    group_id: "g",
    group_name: "g",
    shortcode: "cat",
    content_hash: "h",
    content_type: "image/webp",
    animated: false,
    size_bytes: 1,
    created_by: "u",
  };
  const items = rankEmoji("cat", STANDARD_EMOJI, [custom], [en]);
  assert.equal(items[0]?.kind, "custom");
});

test("the display name is the locale's, falling back through the stack to Unicode", () => {
  const heart = STANDARD_EMOJI.find((e) => e.char === "❤");
  assert.ok(heart);
  assert.equal(emojiDisplayName(heart, [uk, en]), "червоне серце");
  assert.equal(emojiDisplayName(heart, [en]), "red heart");
  assert.equal(emojiDisplayName(heart, []), heart.name);
});

test("every shipped language has a table, and every table covers the whole set", async () => {
  assert.deepEqual([...EMOJI_ANNOTATION_LOCALES], [...supportedLanguageCodes()].sort());
  const known = new Set(STANDARD_EMOJI.map((e) => e.char));
  for (const locale of EMOJI_ANNOTATION_LOCALES) {
    const table = await loadLocale(locale);
    assert.equal(table.size, STANDARD_EMOJI.length, `${locale} covers every entry`);
    for (const [char, annotation] of table) {
      assert.ok(known.has(char), `${locale}: orphan row ${char}`);
      assert.ok(annotation.name.length > 0, `${locale}: ${char} has no name`);
      assert.equal(annotation.name, annotation.name.toLowerCase(), `${locale}: ${char} lowercased`);
    }
  }
});
