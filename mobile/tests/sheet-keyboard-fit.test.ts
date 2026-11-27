/* #2123: with the keyboard up on a short phone, a sheet stays below the status bar and shrinks. */
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = join(dirname(fileURLToPath(import.meta.url)), "..");
const overlay = readFileSync(join(ROOT, "components/chat/SheetOverlay.tsx"), "utf8");
const picker = readFileSync(join(ROOT, "components/emoji/EmojiPickerSheet.tsx"), "utf8");

test("the sheet's room starts below the top safe area", () => {
  assert.match(overlay, /paddingTop: topInset \+ space\.lg/);
});

test("the sheet card can shrink into that room", () => {
  assert.match(overlay, /flexShrink: 1,\s+minHeight: 0,/);
});

test("the emoji picker gives up height rather than overflowing", () => {
  assert.match(picker, /height: 460, flexShrink: 1, minHeight: 0/);
});

test("the category tabs keep their height; only the list shrinks", () => {
  assert.match(picker, /style=\{\{ flexGrow: 0, flexShrink: 0 \}\}/);
});
