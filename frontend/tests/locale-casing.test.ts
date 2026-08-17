/*
 * Locale-aware casing of translated copy (#911).
 *
 * Runs on Node's built-in test runner with native TypeScript type stripping —
 * same arrangement as `direction.test.ts`, and deliberately OUTSIDE `src/` so
 * the production `tsc --noEmit` does not typecheck a file importing
 * `node:test`.
 *
 * A browser test cannot reach this: the bug is invisible in all seven shipped
 * locales, because none of them cases differently from the invariant mapping.
 * It is entirely a question of what happens when one that DOES is added, which
 * is exactly what a unit test over the helper can ask today.
 *
 *   node --test frontend/tests/
 */

import test from "node:test";
import assert from "node:assert/strict";

import { localeUpperCase } from "../src/i18n/languages.ts";

test("Turkish cases its own dotted and dotless i", () => {
  // The whole bug in two assertions. `"aktif".toUpperCase()` is "AKTIF" —
  // a Turkish reader's I is not their İ, so the invariant mapping does not
  // just look wrong, it spells a different letter.
  assert.equal(localeUpperCase("aktif", "tr"), "AKTİF");
  assert.notEqual(localeUpperCase("aktif", "tr"), "aktif".toUpperCase());

  // …and the dotless one goes the other way, which no invariant upcase can
  // produce from an input that has no "ı" of its own.
  assert.equal(localeUpperCase("ırmak", "tr"), "IRMAK");
});

test("the shipped locales are unchanged by the fix", () => {
  // Every locale Pollis ships today cases identically to the invariant
  // mapping, which is why this was not user-visible when it was filed. Pinned
  // so the change cannot have quietly altered what is on screen now.
  for (const [language, word] of [
    ["en", "Active"],
    ["es", "Activo"],
    ["fr", "Révoqué"],
    ["ru", "Активна"],
    ["uk", "Прострочене"],
  ] as const) {
    assert.equal(localeUpperCase(word, language), word.toUpperCase());
  }
});

test("caseless scripts come back exactly as they went in", () => {
  // Arabic and Chinese have no case at all. Upper-casing them is a no-op
  // dressed up as a transform — the reason the ticket calls the whole habit
  // a smell rather than only a Turkish problem.
  for (const [language, word] of [
    ["ar", "نشط"],
    ["zh", "有效"],
  ] as const) {
    assert.equal(localeUpperCase(word, language), word);
  }
});
