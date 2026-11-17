/* #1972: the picker lists each category in CLDR order (the vendored emoji-test.txt order), on desktop and mobile alike. */

import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

import { EMOJI_CATEGORIES, STANDARD_EMOJI } from "../src/components/Emoji/emojiData.ts";

const root = join(dirname(fileURLToPath(import.meta.url)), "..", "..");
const order = JSON.parse(readFileSync(join(root, "scripts/emoji-order.json"), "utf8")) as {
  order: string[];
};
const rank = new Map(order.order.map((key, index) => [String.fromCodePoint(parseInt(key, 16)), index]));

test("People opens on the smileys, not on the lowest codepoints", () => {
  const people = STANDARD_EMOJI.filter((e) => e.category === "people").map((e) => e.char);
  assert.deepEqual(people.slice(0, 4), ["😀", "😃", "😄", "😁"]);
  // The BMP symbols that used to lead the category now sit where CLDR files them.
  for (const early of ["☝", "☹", "❣", "❤"]) {
    assert.ok(people.indexOf(early) > 20, `${early} must not lead People`);
  }
});

test("every category is in CLDR order, unlisted emoji after the listed ones", () => {
  for (const category of EMOJI_CATEGORIES) {
    if (category.id === "flags") {
      continue;
    }
    const ranks = STANDARD_EMOJI.filter((e) => e.category === category.id).map(
      (e) => rank.get(e.char) ?? Number.MAX_SAFE_INTEGER,
    );
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
