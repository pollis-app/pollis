/*
 * Mobile localization plumbing (#1074).
 *
 * The catalogues are the desktop's own files, so what can drift is the
 * mobile-side bookkeeping around them: the language registry, the namespace
 * list, and the generated static resource table Metro needs in place of a
 * glob. Each is pinned here against the directory listing that actually
 * ships, plus the pure resolution rules a device locale goes through.
 *
 *   cd mobile && pnpm test
 */

import test from "node:test";
import assert from "node:assert/strict";
import { readdirSync, readFileSync, statSync } from "node:fs";
import { join, resolve } from "node:path";

import {
  DEFAULT_LANGUAGE,
  SUPPORTED_LANGUAGES,
  languageDirection,
  languageKey,
  localeUpperCase,
  normalizeLanguage,
  resolveDeviceLanguage,
  supportedLanguageCodes,
} from "../i18n/languages.ts";
import { NAMESPACES } from "../i18n/namespaces.ts";

const ROOT = resolve(import.meta.dirname, "../..");
const LOCALES_DIR = join(ROOT, "frontend/src/i18n/locales");

function shippedLocales(): string[] {
  return readdirSync(LOCALES_DIR)
    .filter((entry) => statSync(join(LOCALES_DIR, entry)).isDirectory())
    .sort();
}

test("the language registry matches the shipped catalogue directories", () => {
  assert.deepEqual([...supportedLanguageCodes()].sort(), shippedLocales());
  assert.ok(SUPPORTED_LANGUAGES.some((l) => l.code === DEFAULT_LANGUAGE));
  for (const option of SUPPORTED_LANGUAGES) {
    assert.equal(option.code, option.code.toLowerCase(), `${option.code} is a lowercase base tag`);
    assert.ok(!option.code.includes("-"), `${option.code} carries no subtag`);
  }
});

test("the namespace list matches the English catalogue files", () => {
  const files = readdirSync(join(LOCALES_DIR, "en"))
    .filter((f) => f.endsWith(".json"))
    .map((f) => f.replace(/\.json$/, ""))
    .sort();
  assert.deepEqual([...NAMESPACES].sort(), files);
});

test("the static resource table is what the generator would emit", async () => {
  const { listCatalogues, render } = await import("../../scripts/mobile-i18n-resources.mjs");
  const current = readFileSync(join(ROOT, "mobile/i18n/resources.ts"), "utf8");
  assert.equal(current, render(listCatalogues(LOCALES_DIR)));
});

test("normalizeLanguage reduces device tags to shipped base languages", () => {
  assert.equal(normalizeLanguage("es-MX"), "es");
  assert.equal(normalizeLanguage("zh-Hans-CN"), "zh");
  assert.equal(normalizeLanguage("UK"), "uk");
  assert.equal(normalizeLanguage("pt-BR"), null);
  assert.equal(normalizeLanguage(""), null);
  assert.equal(normalizeLanguage(undefined), null);
});

test("resolveDeviceLanguage takes the first shipped candidate, else English", () => {
  assert.equal(resolveDeviceLanguage(["pt-BR", "ar-EG", "en-US"]), "ar");
  assert.equal(resolveDeviceLanguage(["pt-BR"]), DEFAULT_LANGUAGE);
  assert.equal(resolveDeviceLanguage(undefined), DEFAULT_LANGUAGE);
});

test("direction is rtl only for the Arabic-script locale", () => {
  assert.equal(languageDirection("ar"), "rtl");
  assert.equal(languageDirection("en"), "ltr");
  assert.equal(languageDirection("nope"), "ltr");
});

test("localeUpperCase uses the language's own casing", () => {
  assert.equal(localeUpperCase("istanbul", "tr"), "İSTANBUL");
  assert.equal(localeUpperCase("preferences", "en"), "PREFERENCES");
});

test("storage keys are SecureStore-safe and user-scoped", () => {
  assert.equal(languageKey(null), "pollis-language.device");
  assert.match(languageKey("user:with/odd chars"), /^pollis-language\.user\.[A-Za-z0-9._-]+$/);
  assert.notEqual(languageKey("a"), languageKey("b"));
});
