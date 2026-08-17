/*
 * `.codesight/wiki/ui.md`'s component inventory is generated, and stays so (#933).
 *
 * A generator nobody runs is the same doc problem one step further along: the
 * article would still tell you to regenerate, the command would still exist,
 * and the numbers would still be wrong. So the check runs with the rest of the
 * renderer's tests — adding, deleting or renaming a `.tsx` fails here until the
 * inventory is regenerated, which is a one-command fix the failure names.
 *
 *   node --test frontend/tests/
 */

import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import { generate, collect, existingNotes, renderBlock } from "../../scripts/ui-inventory.mjs";

const ARTICLE = fileURLToPath(new URL("../../.codesight/wiki/ui.md", import.meta.url));

test("the ui.md component inventory matches frontend/src", () => {
  const current = readFileSync(ARTICLE, "utf8");
  assert.equal(
    generate(current),
    current,
    "the inventory in .codesight/wiki/ui.md is stale — run `node scripts/ui-inventory.mjs` " +
      "and commit the result. Do not hand-patch it.",
  );
});

test("the inventory covers every .tsx under frontend/src exactly once", () => {
  const groups = collect();
  const paths = [...groups.values()].flat().map((e) => e.path);
  assert.equal(new Set(paths).size, paths.length, "a file was listed twice");

  const article = readFileSync(ARTICLE, "utf8");
  for (const path of paths) {
    assert.ok(article.includes(`\`${path}\``), `${path} is missing from the article`);
  }

  // The headline count is the same number, not a second hand-maintained one —
  // the drift #933 was filed for was exactly a count that disagreed with its
  // own list.
  assert.match(
    article,
    new RegExp(`\\*\\*${paths.length} \`\\.tsx\` files\\*\\*`),
    `the article's file count disagrees with the ${paths.length} files it lists`,
  );
});

test("hand-written notes survive regeneration", () => {
  const article = readFileSync(ARTICLE, "utf8");
  const notes = existingNotes(article);

  // Non-empty on purpose: if the note-carrying entries were ever all deleted,
  // this test would otherwise keep passing while proving nothing.
  assert.ok(
    notes.size >= 10,
    `expected the inventory to still carry per-component notes, found ${notes.size}`,
  );
  assert.ok(notes.has("EmptyState"), "a known annotated entry lost its note");

  // A regenerated block still contains them, verbatim.
  const regenerated = renderBlock(collect(), notes);
  for (const [name, note] of notes) {
    assert.ok(
      regenerated.includes(note.trim()),
      `${name}'s note was dropped by a regeneration`,
    );
  }
});
