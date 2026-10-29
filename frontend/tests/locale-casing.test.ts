/* Locale-aware casing (#911); invisible in shipped locales, so only a unit test can ask. */

import test from "node:test";
import assert from "node:assert/strict";

import { localeUpperCase } from "../src/i18n/languages.ts";

test("Turkish cases its own dotted and dotless i", () => {
  // Turkish: "aktif" uppercases to "AKTİF", not "AKTIF".
  assert.equal(localeUpperCase("aktif", "tr"), "AKTİF");
  assert.notEqual(localeUpperCase("aktif", "tr"), "aktif".toUpperCase());

  assert.equal(localeUpperCase("ırmak", "tr"), "IRMAK");
});

test("the shipped locales are unchanged by the fix", () => {
  // Shipped locales case like the invariant mapping; pinned.
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
  // Caseless scripts: a no-op.
  for (const [language, word] of [
    ["ar", "نشط"],
    ["zh", "有效"],
  ] as const) {
    assert.equal(localeUpperCase(word, language), word);
  }
});
