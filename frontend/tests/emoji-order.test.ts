/* #1972: the picker lists each category in CLDR order (the vendored emoji-test.txt order), on desktop and mobile alike. */

import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

import { EMOJI_CATEGORIES, STANDARD_EMOJI } from "../src/components/Emoji/emojiData.ts";

const root = join(dirname(fileURLToPath(import.meta.url)), "..", "..");
// The file's own order, keyed like the table: a single codepoint bare, a sequence as listed.
const rank = new Map<string, number>();
for (const line of readFileSync(join(root, "scripts/emoji-test.txt"), "utf8").split("\n")) {
  const match = /^([0-9A-F ]+?)\s*; (fully-qualified|component)\s/.exec(line);
  if (!match) {
    continue;
  }
  const emoji = String.fromCodePoint(...match[1].split(" ").map((cp) => parseInt(cp, 16)));
  const bare = emoji.replaceAll("\u{FE0F}", "");
  const key = Array.from(bare).length === 1 ? bare : emoji;
  if (!rank.has(key)) {
    rank.set(key, rank.size);
  }
}

test("People opens on the smileys, not on the lowest codepoints", () => {
  const people = STANDARD_EMOJI.filter((e) => e.category === "people").map((e) => e.char);
  assert.deepEqual(people.slice(0, 4), ["😀", "😃", "😄", "😁"]);
  // The BMP symbols that used to lead the category now sit where CLDR files them.
  for (const early of ["☝", "☹", "❣", "❤"]) {
    assert.ok(people.indexOf(early) > 20, `${early} must not lead People`);
  }
});

test("every category is in CLDR order", () => {
  for (const category of EMOJI_CATEGORIES) {
    const ranks = STANDARD_EMOJI.filter((e) => e.category === category.id).map((e) => {
      const index = rank.get(e.char);
      assert.ok(index !== undefined, `${e.char} is not in emoji-test.txt`);
      return index;
    });
    for (let i = 1; i < ranks.length; i += 1) {
      assert.ok(ranks[i - 1] <= ranks[i], `${category.id} is out of order at position ${i}`);
    }
  }
});

test("desktop and mobile carry the same generated table", () => {
  const desktop = readFileSync(join(root, "frontend/src/components/Emoji/emojiData.ts"), "utf8");
  const mobile = readFileSync(join(root, "mobile/components/emoji/emojiData.ts"), "utf8");
  assert.equal(mobile, desktop);
});
