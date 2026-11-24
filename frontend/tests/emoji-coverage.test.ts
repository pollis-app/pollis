/* #2094: the picker table is every fully-qualified emoji in the vendored emoji-test.txt (Emoji 16.0), sequences and skin tones included. */

import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

import { STANDARD_EMOJI, SKIN_TONES, applySkinTone } from "../src/components/Emoji/emojiData.ts";
import { buildEmojiAnnotations } from "../src/components/Emoji/emojiAnnotations.ts";
import enRows from "../src/components/Emoji/annotations/en.ts";

const root = join(dirname(fileURLToPath(import.meta.url)), "..", "..");
const VS16 = "\u{FE0F}";
const TONES = new Set(SKIN_TONES.slice(1));

interface Line {
  emoji: string;
  status: string;
}

const lines: Line[] = [];
for (const line of readFileSync(join(root, "scripts/emoji-test.txt"), "utf8").split("\n")) {
  const match = /^([0-9A-F ]+?)\s*; ([a-z-]+)\s/.exec(line);
  if (match) {
    const emoji = String.fromCodePoint(...match[1].split(" ").map((cp) => parseInt(cp, 16)));
    lines.push({ emoji, status: match[2] });
  }
}
const fullyQualified = lines.filter((l) => l.status === "fully-qualified").map((l) => l.emoji);
const fullyQualifiedSet = new Set(fullyQualified);
const hasTone = (emoji: string) => Array.from(emoji).some((c) => TONES.has(c));
const hairStyles = lines
  .filter((l) => l.status === "component" && !TONES.has(l.emoji))
  .map((l) => l.emoji);

/** How the table spells an emoji-test.txt form: a single codepoint bare, a sequence as listed. */
function tableChar(emoji: string): string {
  const bare = emoji.replaceAll(VS16, "");
  return Array.from(bare).length === 1 ? bare : emoji;
}

const byChar = new Map(STANDARD_EMOJI.map((e) => [e.char, e]));

test("the file is the vendored Emoji 16.0 one", () => {
  const meta = JSON.parse(readFileSync(join(root, "scripts/emoji-test.json"), "utf8")) as {
    _version: string;
  };
  assert.equal(meta._version, "16.0");
  // The file's own tally, so a truncated copy fails here rather than shrinking the table.
  const text = readFileSync(join(root, "scripts/emoji-test.txt"), "utf8");
  assert.match(text, /^# Version: 16\.0$/m);
  assert.match(text, new RegExp(`^# fully-qualified : ${fullyQualified.length}$`, "m"));
});

test("every fully-qualified emoji without a skin tone is in the table, and nothing else is", () => {
  const expected = fullyQualified.filter((e) => !hasTone(e)).map(tableChar);
  assert.equal(STANDARD_EMOJI.length, expected.length + hairStyles.length);
  assert.equal(STANDARD_EMOJI.length, 1910);
  for (const char of [...expected, ...hairStyles]) {
    assert.ok(byChar.has(char), `${char} is missing`);
  }
  assert.equal(byChar.size, STANDARD_EMOJI.length, "every char is listed once");
});

test("the emoji the issue named are there, with the right category", () => {
  const cases: [string, string][] = [
    ["\u{1FAE1}", "people"], // 🫡 saluting face
    ["\u{1FAE0}", "people"], // 🫠 melting face
    ["🧑‍💻", "people"],
    ["❤️‍🔥", "people"],
    ["😮‍💨", "people"],
    ["🙂‍↔️", "people"],
    ["🐦‍🔥", "nature"],
    ["\u{1FAE9}", "people"], // 🫩 face with bags under eyes, Emoji 16.0
    ["🇺🇦", "flags"],
    ["🇺🇳", "flags"],
    ["🏴󠁧󠁢󠁳󠁣󠁴󠁿", "flags"],
    ["🏳️‍🌈", "flags"],
    ["#️⃣", "symbols"],
  ];
  for (const [char, category] of cases) {
    assert.equal(byChar.get(char)?.category, category, char);
  }
  assert.deepEqual(byChar.get("\u{1FAE1}")?.shortcodes, ["saluting_face"]);
  assert.deepEqual(byChar.get("❤️‍🔥")?.shortcodes, ["heart_on_fire"]);
  assert.equal(byChar.get("🇺🇦")?.name, "ukraine flag");
});

test("a sequence keeps its selectors, a single codepoint stays bare", () => {
  // Reactions and recents stored before #2094 keep matching.
  assert.ok(byChar.has("\u{2764}"));
  assert.ok(!byChar.has("\u{2764}\u{FE0F}"));
  assert.ok(byChar.has("\u{2764}\u{FE0F}\u{200D}\u{1F525}"));
  assert.ok(!byChar.has("\u{2764}\u{200D}\u{1F525}"));
  for (const emoji of STANDARD_EMOJI) {
    if (Array.from(emoji.char).length > 1 && !hairStyles.includes(emoji.char)) {
      assert.ok(fullyQualifiedSet.has(emoji.char), `${emoji.char} is not fully-qualified`);
    }
  }
});

test("every toned form the picker can make is one Unicode lists", () => {
  for (const emoji of STANDARD_EMOJI) {
    for (let tone = 1; tone < SKIN_TONES.length; tone += 1) {
      const toned = applySkinTone(emoji, tone);
      if (emoji.tonable) {
        assert.ok(fullyQualifiedSet.has(toned), `${emoji.char} tone ${tone} gave ${toned}`);
      } else {
        assert.equal(toned, emoji.char);
      }
    }
  }
});

test("every uniform-tone form Unicode lists is reachable from its base", () => {
  const reachable = new Set<string>();
  for (const emoji of STANDARD_EMOJI) {
    for (let tone = 1; tone < SKIN_TONES.length; tone += 1) {
      reachable.add(applySkinTone(emoji, tone));
    }
  }
  for (const emoji of fullyQualified) {
    const tones = new Set(Array.from(emoji).filter((c) => TONES.has(c)));
    if (tones.size === 1) {
      assert.ok(reachable.has(emoji), `${emoji} cannot be picked`);
    }
  }
});

test("skin tones on sequences, including two-person ones", () => {
  const tone = (char: string, index: number) => applySkinTone(byChar.get(char)!, index);
  assert.equal(tone("👍", 3), "👍🏽");
  assert.equal(tone("🧑‍💻", 2), "🧑🏼‍💻");
  assert.equal(tone("🏃‍♀️", 1), "🏃🏻‍♀️");
  assert.equal(tone("⛹️‍♂️", 4), "⛹🏾‍♂️");
  assert.equal(tone("🧑‍🤝‍🧑", 5), "🧑🏿‍🤝‍🧑🏿");
  assert.equal(tone("👩‍❤️‍👨", 3), "👩🏽‍❤️‍👨🏽");
  assert.equal(tone("🤝", 1), "🤝🏻");
  // Not modifier bases: no tone is glued on.
  assert.equal(byChar.get("🧐")?.tonable, false);
  assert.equal(byChar.get("\u{1FAE1}")?.tonable, false);
});

test("English annotations name every entry, so other locales can fall back to them", () => {
  const en = buildEmojiAnnotations(enRows);
  for (const emoji of STANDARD_EMOJI) {
    assert.ok(en.has(emoji.char), `${emoji.char} has no English annotation`);
  }
  assert.equal(en.get("\u{1FAE1}")?.name, "saluting face");
  assert.equal(en.get("❤️‍🔥")?.name, "heart on fire");
});
