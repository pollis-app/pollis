/* #2094: mobile's picker table carries the full Emoji 16.0 set; frontend/tests/emoji-coverage.test.ts checks it against emoji-test.txt in depth. */

import test from "node:test";
import assert from "node:assert/strict";

import { STANDARD_EMOJI, applySkinTone } from "../components/emoji/emojiData.ts";

const byChar = new Map(STANDARD_EMOJI.map((e) => [e.char, e]));

test("the emoji #2094 named are in the table", () => {
  assert.equal(STANDARD_EMOJI.length, 1910);
  for (const char of ["\u{1FAE1}", "\u{1FAE0}", "🧑‍💻", "❤️‍🔥", "😮‍💨", "🐦‍🔥", "🇺🇦", "🏴󠁧󠁢󠁷󠁬󠁳󠁿"]) {
    assert.ok(byChar.has(char), `${char} is missing`);
  }
});

test("skin tones reach sequences, including two-person ones", () => {
  assert.equal(applySkinTone(byChar.get("🧑‍💻")!, 3), "🧑🏽‍💻");
  assert.equal(applySkinTone(byChar.get("🧑‍🤝‍🧑")!, 1), "🧑🏻‍🤝‍🧑🏻");
  assert.equal(applySkinTone(byChar.get("\u{1FAE1}")!, 3), "\u{1FAE1}");
});
