/* The layout direction is written only once the stored language is known: Android's Fabric re-reads it on every root measure. */
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = join(dirname(fileURLToPath(import.meta.url)), "..");
const source = readFileSync(join(ROOT, "i18n/index.ts"), "utf8");

test("i18n/index.ts does not write the layout direction at module load", () => {
  // A top-level statement starts at column 0; calls inside functions are indented.
  const topLevel = source
    .split("\n")
    .filter((line) => /^(applyLayoutDirection|I18nManager\.(forceRTL|allowRTL))\(/.test(line));
  assert.deepEqual(topLevel, [], "direction must not be written before hydrateLanguage resolves");
});

test("hydrateLanguage applies the direction after reading the stored language", () => {
  const body = source.match(/export async function hydrateLanguage\(\)[^{]*\{([\s\S]*?)\n\}/);
  assert.ok(body, "hydrateLanguage must exist");
  const load = body[1].indexOf("loadDeviceLanguage(");
  const apply = body[1].indexOf("applyLayoutDirection(");
  assert.ok(load >= 0 && apply > load, "hydrateLanguage must apply the direction after loading");
});
