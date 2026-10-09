/*
 * Multi-line messages (#1247). Return in the composer inserts a newline, so a
 * draft can span lines; these pin that the line breaks survive the send
 * normalisation and the render-side splitting (mentions, custom emoji), so
 * the row shows the message exactly as typed, as desktop does.
 */
import test from "node:test";
import assert from "node:assert/strict";

import { outgoingText } from "../lib/messageText.ts";
import { findMentions } from "../lib/mentions.ts";
import { splitEmojiSegments } from "../components/emoji/emojiTokens.ts";

test("only the ends of a draft are trimmed", () => {
  assert.equal(outgoingText("  first line\nsecond line \n"), "first line\nsecond line");
});

test("internal line breaks, blank lines and indentation are kept", () => {
  const draft = "one\n\n  two\n\tthree";
  assert.equal(outgoingText(draft), draft);
});

test("a whitespace-only draft sends as empty", () => {
  assert.equal(outgoingText("\n \n\t"), "");
});

test("a mention at the start of a later line is found", () => {
  const text = "hi\n@dana see this";
  const mentions = findMentions(text);
  assert.equal(mentions.length, 1);
  assert.equal(mentions[0].name, "dana");
  assert.equal(text.slice(mentions[0].start, mentions[0].end), "@dana");
});

test("emoji splitting keeps the newlines between tokens", () => {
  const hash = "a".repeat(64);
  const text = `line one\n<:party:${hash}>\nline two`;
  const segments = splitEmojiSegments(text);
  const textParts = segments
    .filter((s) => s.kind === "text")
    .map((s) => (s.kind === "text" ? s.text : ""));
  assert.deepEqual(textParts, ["line one\n", "\nline two"]);
});
