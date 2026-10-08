/*
 * The layout direction is only written once the stored language is known.
 *
 * Android's Fabric re-reads the persisted `I18nUtil` direction on every root
 * measure (ReactSurfaceImpl.updateLayoutSpecs), not once at launch. A
 * module-load `forceRTL(false)` from the device-locale guess, before
 * `hydrateLanguage` has read the user's stored Arabic pick, therefore landed
 * in that window and un-mirrored Yoga for the whole launch, while JS (which
 * had already read `isRTL`) still flipped direction-aware icons. iOS reads the
 * direction once, so it never showed. `i18n/index.ts` imports react-native and
 * cannot load under plain node, so the invariant is pinned on its source.
 */
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
