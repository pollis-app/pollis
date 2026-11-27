/* Script subtags (#1725): OS tags map onto `<base>-<Script>` codes for the languages that ship per script, and nothing else moves. */

import test from "node:test";
import assert from "node:assert/strict";

import { REGISTERED_CODE, SCRIPT_VARIANTS, matchLanguageTag } from "../src/i18n/languageTag.ts";
import { normalizeLanguage, resolveOsLanguage, supportedLanguageCodes } from "../src/i18n/languages.ts";

// A registry with both Punjabi scripts, as L5b will ship it, plus today's locales.
const WITH_PUNJABI = [...supportedLanguageCodes(), "pa-Arab", "pa-Guru"];

test("Punjabi resolves to the script its readers use", () => {
  const table: Array<[string, string | null]> = [
    ["pa-PK", "pa-Arab"],
    ["pa_PK", "pa-Arab"],
    ["pa-Arab", "pa-Arab"],
    ["pa-arab", "pa-Arab"],
    ["pa-Arab-PK", "pa-Arab"],
    ["pa-Arab-IN", "pa-Arab"],
    ["pa-IN", "pa-Guru"],
    ["pa-Guru", "pa-Guru"],
    ["pa-Guru-IN", "pa-Guru"],
    ["pa-Guru-PK", "pa-Guru"],
    ["pa", "pa-Guru"],
    ["PA", "pa-Guru"],
    ["pa-GB", "pa-Guru"],
  ];
  for (const [tag, want] of table) {
    assert.equal(matchLanguageTag(tag, WITH_PUNJABI), want, tag);
  }
});

test("a reader is never handed the other script", () => {
  // Only the other script registered: the registry minus this script, plus the other one.
  const onlyGuru = [...supportedLanguageCodes().filter((c) => c !== "pa-Arab"), "pa-Guru"];
  const onlyArab = [...supportedLanguageCodes().filter((c) => c !== "pa-Guru"), "pa-Arab"];
  assert.equal(matchLanguageTag("pa-PK", onlyGuru), null);
  assert.equal(matchLanguageTag("pa-Arab", onlyGuru), null);
  assert.equal(matchLanguageTag("pa-IN", onlyArab), null);
});

test("every other language keeps the base-tag rule", () => {
  const table: Array<[string, string | null]> = [
    ["zh-Hans-CN", "zh"],
    ["zh-Hant-TW", "zh"],
    ["zh_CN", "zh"],
    ["ja-JP", "ja"],
    ["ja-Jpan-JP", "ja"],
    ["ko-KR", "ko"],
    ["ko-Kore-KR", "ko"],
    ["ar-EG", "ar"],
    ["es-419", "es"],
    ["pt-BR", "pt"],
    ["pt-PT", "pt"],
    ["pt-AO", "pt"],
    ["pt-MZ", "pt"],
    ["sw-KE", "sw"],
    ["ha-Latn-NG", "ha"],
    ["am-ET", "am"],
    ["yo-NG", "yo"],
    ["zu-ZA", "zu"],
    ["de-DE", null],
    ["", null],
  ];
  for (const [tag, want] of table) {
    assert.equal(matchLanguageTag(tag, WITH_PUNJABI), want, tag);
  }
});

test("every stored preference for an existing locale still resolves to itself", () => {
  // These are the values storage.ts wrote before script codes existed: the registered codes, lowercased.
  for (const stored of ["en", "es", "uk", "fr", "ru", "zh", "ar"]) {
    assert.equal(normalizeLanguage(stored), stored, stored);
    assert.equal(normalizeLanguage(` ${stored.toUpperCase()} `), stored, stored);
  }
  assert.equal(normalizeLanguage(null), null);
  assert.equal(normalizeLanguage("xx"), null);
});

test("Korean and Japanese are auto-detected from OS tags", () => {
  assert.equal(normalizeLanguage("ko-KR"), "ko");
  assert.equal(normalizeLanguage("ja-JP"), "ja");
  assert.equal(normalizeLanguage("ja"), "ja");
});

test("African locales are auto-detected from OS tags; every Portuguese region lands on the one pt catalogue (#1727)", () => {
  for (const [tag, want] of [
    ["sw-KE", "sw"],
    ["sw-TZ", "sw"],
    ["pt-AO", "pt"],
    ["pt-MZ", "pt"],
    ["pt-PT", "pt"],
    ["pt-BR", "pt"],
    ["ha-NG", "ha"],
    ["am-ET", "am"],
    ["yo-NG", "yo"],
    ["yo-BJ", "yo"],
    ["zu-ZA", "zu"],
  ] as const) {
    assert.equal(normalizeLanguage(tag), want, tag);
    assert.equal(resolveOsLanguage([tag]), want, tag);
  }
});

test("registered codes have the shape i18next keeps, and script codes are declared", () => {
  for (const code of supportedLanguageCodes()) {
    assert.match(code, REGISTERED_CODE, code);
    // i18next canonicalizes through Intl; a code it would rewrite never matches `supportedLngs`.
    assert.equal(Intl.getCanonicalLocales(code)[0], code, code);
    const [base, script] = code.split("-");
    if (script) {
      assert.ok(SCRIPT_VARIANTS[base], `${code}: ${base} is not in SCRIPT_VARIANTS`);
    }
  }
  for (const [base, variants] of Object.entries(SCRIPT_VARIANTS)) {
    for (const script of [variants.fallback, ...Object.values(variants.byRegion)]) {
      assert.equal(Intl.getCanonicalLocales(`${base}-${script}`)[0], `${base}-${script}`);
    }
  }
});

test("South Asian locales are auto-detected from OS tags (#1726)", () => {
  assert.equal(normalizeLanguage("ur-PK"), "ur");
  assert.equal(normalizeLanguage("hi-IN"), "hi");
  assert.equal(normalizeLanguage("pa-PK"), "pa-Arab");
  assert.equal(normalizeLanguage("ps-AF"), "ps");
  assert.equal(normalizeLanguage("bn-BD"), "bn");
  assert.equal(normalizeLanguage("mr-IN"), "mr");
  assert.equal(normalizeLanguage("te-IN"), "te");
  assert.equal(normalizeLanguage("ta-LK"), "ta");
  assert.equal(normalizeLanguage("pa-IN"), "pa-Guru");
});

test("Persian is auto-detected from OS tags (#2131)", () => {
  assert.equal(normalizeLanguage("fa"), "fa");
  assert.equal(normalizeLanguage("fa-IR"), "fa");
  assert.equal(normalizeLanguage("fa-AF"), "fa");
  assert.equal(normalizeLanguage("fa-Arab-IR"), "fa");
});
