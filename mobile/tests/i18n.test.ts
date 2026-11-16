/* Mobile localization plumbing (#1074): the registry, namespaces and generated resource table vs what ships. */

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
  usesNastaliq,
} from "../i18n/languages.ts";
import { REGISTERED_CODE, SCRIPT_VARIANTS, matchLanguageTag } from "../i18n/languageTag.ts";
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
    // A base tag, or `<base>-<Script>` for a language shipped per script (#1725).
    assert.match(option.code, REGISTERED_CODE, option.code);
    const [base, script] = option.code.split("-");
    assert.ok(!script || SCRIPT_VARIANTS[base], `${option.code}: ${base} is not in SCRIPT_VARIANTS`);
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

test("languageTag.ts is a byte copy of the desktop one", () => {
  assert.equal(
    readFileSync(join(ROOT, "mobile/i18n/languageTag.ts"), "utf8"),
    readFileSync(join(ROOT, "frontend/src/i18n/languageTag.ts"), "utf8"),
  );
});

test("the mobile registry matches desktop's code, endonym and direction for every language", () => {
  const desktop = readFileSync(join(ROOT, "frontend/src/i18n/languages.ts"), "utf8");
  const rows = [...desktop.matchAll(/\{ code: "([^"]+)", label: "([^"]+)", dir: "(ltr|rtl)" \}/g)].map((m) => `${m[1]}|${m[2]}|${m[3]}`);
  assert.deepEqual(SUPPORTED_LANGUAGES.map((l) => `${l.code}|${l.label}|${l.dir}`), rows);
});

test("script subtags: Punjabi follows its script, everything else its base tag, stored choices unchanged (#1725)", () => {
  const codes = [...supportedLanguageCodes(), "pa-Arab", "pa-Guru"];
  for (const [tag, want] of [
    ["pa-PK", "pa-Arab"],
    ["pa-Arab-IN", "pa-Arab"],
    ["pa-IN", "pa-Guru"],
    ["pa", "pa-Guru"],
    ["zh-Hant-TW", "zh"],
    ["ja-JP", "ja"],
    ["ko-KR", "ko"],
  ] as const) {
    assert.equal(matchLanguageTag(tag, codes), want, tag);
  }
  assert.equal(matchLanguageTag("pa-PK", [...supportedLanguageCodes().filter((c) => c !== "pa-Arab"), "pa-Guru"]), null);
  for (const stored of ["en", "es", "uk", "fr", "ru", "zh", "ar"]) {
    assert.equal(normalizeLanguage(stored), stored);
  }
  assert.equal(resolveDeviceLanguage(["ja-JP"]), "ja");
  assert.equal(resolveDeviceLanguage(["ko-KR"]), "ko");
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

test("only the Nastaliq languages get the taller type line heights (#1726)", () => {
  assert.equal(usesNastaliq("ur"), true);
  assert.equal(usesNastaliq("pa-Arab"), true);
  // Pashto and Arabic are Naskh, which fits the Latin-sized steps.
  for (const code of ["ps", "ar", "pa-Guru", "hi", "en", null]) {
    assert.equal(usesNastaliq(code), false, String(code));
  }
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

test("South Asian locales are auto-detected from OS tags (#1726)", () => {
  assert.equal(resolveDeviceLanguage(["ur-PK"]), "ur");
  assert.equal(resolveDeviceLanguage(["hi-IN"]), "hi");
  assert.equal(resolveDeviceLanguage(["pa-PK"]), "pa-Arab");
  assert.equal(resolveDeviceLanguage(["ps-AF"]), "ps");
  assert.equal(resolveDeviceLanguage(["bn-BD"]), "bn");
  assert.equal(resolveDeviceLanguage(["mr-IN"]), "mr");
  assert.equal(resolveDeviceLanguage(["te-IN"]), "te");
});
