/* Shortcode from a dropped file's name; any non-empty result must pass `SHORTCODE_RE`. */

import test from "node:test";
import assert from "node:assert/strict";

import {
  SHORTCODE_RE,
  emojiMimeType,
  fileName,
  isSupportedEmojiFile,
  sanitizeShortcode,
  shortcodeFromFileName,
} from "../src/components/Emoji/emojiShortcode.ts";

test("a filename becomes a usable shortcode", () => {
  assert.equal(shortcodeFromFileName("/home/mia/party_parrot.gif"), "party_parrot");
  assert.equal(shortcodeFromFileName("C:\\Users\\mia\\Ship It.PNG"), "ship_it");
  assert.equal(shortcodeFromFileName("Party Parrot (1).GIF"), "party_parrot_1");
  assert.equal(shortcodeFromFileName("🎉 confetti!.webp"), "confetti");
});

test("everything it derives is a shortcode the DS would accept", () => {
  for (const name of [
    "party_parrot.gif",
    "Ship It.PNG",
    "Party Parrot (1).GIF",
    "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.png",
    "a b c d e f g h i j k l m n o p q r s t u v w x y z.png",
  ]) {
    const code = shortcodeFromFileName(name);
    assert.match(code, SHORTCODE_RE, `${name} derived ${code}`);
  }
});

test("truncation never leaves a trailing underscore", () => {
  // A cut landing on a separator must not leave a trailing `_`.
  const code = shortcodeFromFileName("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa b.png");
  assert.equal(code.length <= 32, true);
  assert.equal(code.endsWith("_"), false);
  assert.match(code, SHORTCODE_RE);
});

test("a name with nothing usable in it comes back empty", () => {
  assert.equal(shortcodeFromFileName("###.png"), "");
  assert.equal(sanitizeShortcode("___"), "");
});

test("sanitising collapses illegal runs rather than deleting them", () => {
  assert.equal(sanitizeShortcode("Party  --  Parrot"), "party_parrot");
  assert.equal(sanitizeShortcode("_leading_and_trailing_"), "leading_and_trailing");
  assert.equal(sanitizeShortcode("ALREADY_ok_123"), "already_ok_123");
});

test("only the image types the picker offers are accepted", () => {
  for (const path of ["a.png", "a.JPG", "a.jpeg", "a.gif", "a.webp"]) {
    assert.equal(isSupportedEmojiFile(path), true, path);
  }
  for (const path of ["a.pdf", "a.svg", "a.mp4", "noextension", ".gitignore"]) {
    assert.equal(isSupportedEmojiFile(path), false, path);
  }
});

test("the preview blob gets the type its extension implies", () => {
  assert.equal(emojiMimeType("/tmp/a.gif"), "image/gif");
  assert.equal(emojiMimeType("/tmp/a.JPG"), "image/jpeg");
  assert.equal(emojiMimeType("/tmp/a.webp"), "image/webp");
});

test("filenames are split on either platform's separator", () => {
  assert.equal(fileName("/home/mia/a.png"), "a.png");
  assert.equal(fileName("C:\\Users\\mia\\a.png"), "a.png");
  assert.equal(fileName("a.png"), "a.png");
});
