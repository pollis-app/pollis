/* The emoji picker's non-English annotation tables ship as assets, not modules (#2188), and the
 * app carries only the Geist weights it registers. A table that cannot be read leaves English. */

import test from "node:test";
import assert from "node:assert/strict";
import { readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { EMOJI_ANNOTATION_LOCALES, loadEmojiAnnotationRows } from "../components/emoji/annotations/index.ts";
import { parseEmojiTable, readEmojiTable } from "../components/emoji/emojiTableAsset.ts";

const MOBILE = new URL("..", import.meta.url).pathname;
const ROOT = join(MOBILE, "..");
const DIR = join(MOBILE, "components/emoji/annotations");
const src = (p: string) => readFileSync(join(MOBILE, p), "utf8");

test("only English is a module; every other locale is a .emojitable asset with the desktop rows", async () => {
  const files = readdirSync(DIR).sort();
  assert.deepEqual(files.filter((f) => f.endsWith(".ts")), ["en.ts", "index.ts"]);
  for (const locale of EMOJI_ANNOTATION_LOCALES) {
    if (locale === "en") {
      continue;
    }
    const rows = parseEmojiTable(readFileSync(join(DIR, `${locale}.emojitable`), "utf8"));
    // Desktop keeps its per-locale chunk; both are generated from the same vendored data.
    const desktop = await import(join(ROOT, `frontend/src/components/Emoji/annotations/${locale}.ts`));
    assert.deepEqual(rows, desktop.default, `${locale}: the same rows as desktop's table`);
  }
  // English is byte-identical to desktop's module.
  assert.equal(src("components/emoji/annotations/en.ts"), readFileSync(join(ROOT, "frontend/src/components/Emoji/annotations/en.ts"), "utf8"));
});

test("Metro ships the tables as assets, under their own extension and never json", () => {
  const metro = src("metro.config.js");
  assert.match(metro, /config\.resolver\.assetExts = \[\.\.\.config\.resolver\.assetExts, "emojitable"\]/);
  assert.ok(!/assetExts[^\n]*"json"/.test(metro), "json must stay a source extension");
  const index = src("components/emoji/annotations/index.ts");
  for (const locale of EMOJI_ANNOTATION_LOCALES) {
    if (locale !== "en") {
      assert.ok(index.includes(`readEmojiTable(() => require("./${locale}.emojitable"))`), locale);
    }
  }
  assert.ok(index.includes('return import("./en").then((m) => m.default);'));
});

test("useEmojiAnnotations is untouched: still the byte copy of desktop's", () => {
  assert.equal(
    src("components/emoji/useEmojiAnnotations.ts"),
    readFileSync(join(ROOT, "frontend/src/components/Emoji/useEmojiAnnotations.ts"), "utf8"),
  );
  // Its stack keeps the tables that loaded and drops a failed one (so English stays), and a
  // failure is not cached, so the next open reads again.
  const hook = src("components/emoji/useEmojiAnnotations.ts");
  assert.match(hook, /pendingTables\.delete\(locale\);\n\s+return null;/);
  assert.match(hook, /tables\.filter\(\(table\): table is EmojiAnnotations => table !== null\)/);
  assert.match(hook, /if \(stack\.length === locales\.length\) \{\n\s+resolvedStacks\.set/);
});

test("a failed asset read rejects (never throws), and English still loads", async () => {
  // Under node there is no Metro `require`, so the asset path fails the way a missing asset does.
  const fr = loadEmojiAnnotationRows("fr");
  assert.ok(fr instanceof Promise, "a promise, not a synchronous throw");
  await assert.rejects(fr);
  // English is a bundled module, reached without any asset read (`import("./en")` is Metro's).
  const en = (await import("../components/emoji/annotations/en.ts")).default;
  assert.ok(en.length > 1000);
  // Every failure mode of the reader rejects.
  await assert.rejects(readEmojiTable(() => 1, () => Promise.reject(new Error("offline"))));
  await assert.rejects(readEmojiTable(() => 1, async () => "not json"));
  await assert.rejects(readEmojiTable(() => 1, async () => '[["a", "b"]]'));
  await assert.rejects(readEmojiTable(() => {
    throw new Error("no asset");
  }, async () => "[]"));
  // And a read that never answers is given up on.
  await assert.rejects(readEmojiTable(() => 1, () => new Promise<string>(() => {}), 20), /timed out/);
  // A good read parses.
  assert.deepEqual(await readEmojiTable(() => 1, async () => '[["😀", "grin", "a|b"]]'), [["😀", "grin", "a|b"]]);
});

test("only the Geist weights the app registers are imported, each from its own entry point", () => {
  const layout = src("app/_layout.tsx");
  assert.ok(!/from "@expo-google-fonts\/geist";/.test(layout), "never the package index (it requires all 18 TTFs)");
  const weights = [...layout.matchAll(/import \{ (Geist_\w+) \} from "@expo-google-fonts\/geist\/(\w+)";/g)];
  assert.ok(weights.length >= 4);
  const registered = layout.slice(layout.indexOf("useFonts({"), layout.indexOf("});", layout.indexOf("useFonts({")));
  for (const [, name, dir] of weights) {
    assert.equal(name, `Geist_${dir}`);
    assert.ok(registered.includes(name), `${name} is registered`);
  }
});
