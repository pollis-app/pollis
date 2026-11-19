/* A filled Chip draws its glyph in the label's ink (onDanger / onAccent), as Button does: never a white or accent icon on the fill. */
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = join(dirname(fileURLToPath(import.meta.url)), "..");
const read = (rel: string) => readFileSync(join(ROOT, rel), "utf8");

test("a filled Chip tints its leading glyph with the label colour", () => {
  const ui = read("components/ui.tsx");
  const chip = ui.slice(ui.indexOf("export function Chip("), ui.indexOf("export function Button("));
  assert.match(chip, /dangerFill \|\| accentFill \? \([\s\S]*?tintGlyph\(leading, fg\)/);
  assert.match(chip, /dangerFill\s*\?\s*semantic\.onDanger/);
});

test("the channel Delete chip leaves its icon colour to the Chip", () => {
  const src = read("app/group/settings.tsx");
  assert.match(src, /leading=\{<Icon\.trash size=\{14\} \/>\}/);
});
