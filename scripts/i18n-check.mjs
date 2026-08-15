#!/usr/bin/env node
/**
 * Catalogue integrity check.
 *
 * Every locale is machine-translated, and the failure modes that matter are
 * precisely the ones the type checker and the e2e suite cannot see: a locale
 * whose plural families were copied across from English renders grammatically
 * wrong counts in Russian, Ukrainian and Arabic while passing every other
 * gate we have. So this checks the things that are otherwise invisible:
 *
 *   1. key parity with `en` — nothing missing, nothing orphaned
 *   2. plural families carry EXACTLY the CLDR categories the locale requires,
 *      taken from `Intl.PluralRules` rather than a hand-maintained table
 *   3. interpolation placeholders survive translation in both directions
 *   4. no empty values (an empty string renders as a blank UI slot, which
 *      reads as a bug rather than as missing copy)
 *   5. every `t('ns:key')` call site in the renderer resolves in `en`
 *
 * Run: `node scripts/i18n-check.mjs`
 */

import { readFileSync, readdirSync, statSync } from "node:fs";
import { join, resolve, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const LOCALES_DIR = join(ROOT, "frontend/src/i18n/locales");
const SRC_DIR = join(ROOT, "frontend/src");
const BASE = "en";

const PLURAL_SUFFIXES = ["zero", "one", "two", "few", "many", "other"];

let failures = 0;
function fail(message) {
  failures += 1;
  console.error(`  ✗ ${message}`);
}

/** Flatten `{a:{b:"x"}}` to `{"a.b":"x"}`. */
function flatten(object, prefix = "", out = {}) {
  for (const [key, value] of Object.entries(object)) {
    const path = prefix ? `${prefix}.${key}` : key;
    if (value && typeof value === "object" && !Array.isArray(value)) {
      flatten(value, path, out);
    } else {
      out[path] = value;
    }
  }
  return out;
}

function loadLocale(code) {
  const dir = join(LOCALES_DIR, code);
  const catalogue = {};
  for (const file of readdirSync(dir).filter((f) => f.endsWith(".json"))) {
    const ns = file.replace(/\.json$/, "");
    const parsed = JSON.parse(readFileSync(join(dir, file), "utf8"));
    for (const [key, value] of Object.entries(flatten(parsed))) {
      catalogue[`${ns}:${key}`] = value;
    }
  }
  return catalogue;
}

/**
 * Split a key into its plural family and suffix, or null when it is not a
 * plural member. `chat:preview.attachments_one` → family `…attachments`.
 */
function splitPlural(key) {
  const match = key.match(/^(.*)_(zero|one|two|few|many|other)$/);
  return match ? { family: match[1], suffix: match[2] } : null;
}

function placeholdersOf(value) {
  if (typeof value !== "string") {
    return [];
  }
  return [...value.matchAll(/\{\{\s*([\w.]+)[^}]*\}\}/g)]
    .map((m) => m[1])
    .sort();
}

const localeCodes = readdirSync(LOCALES_DIR).filter((entry) =>
  statSync(join(LOCALES_DIR, entry)).isDirectory(),
);
const base = loadLocale(BASE);
const baseKeys = Object.keys(base);

// Families in the base catalogue, and the members each locale must supply.
const baseFamilies = new Set();
for (const key of baseKeys) {
  const plural = splitPlural(key);
  if (plural) {
    baseFamilies.add(plural.family);
  }
}

console.log(
  `base ${BASE}: ${baseKeys.length} keys, ${baseFamilies.size} plural families\n`,
);

for (const code of localeCodes.filter((c) => c !== BASE)) {
  console.log(`${code}:`);
  const locale = loadLocale(code);

  // Expected CLDR categories for THIS locale, straight from the platform.
  const rules = new Intl.PluralRules(code);
  const categories = rules.resolvedOptions().pluralCategories;
  const declared = PLURAL_SUFFIXES.filter((s) => categories.includes(s));

  // Categories reachable by a count this UI can actually render. Modern CLDR
  // gives Spanish and French a `many` category, but only for magnitudes at or
  // above a million — and nothing here counts members, attachments or minutes
  // that high. Missing a reachable category (Russian `few` at 2, Arabic `two`)
  // is a real defect that renders wrong grammar to real users; missing an
  // unreachable one is a note, since i18next falls back to `other` anyway.
  const reachable = new Set();
  // How many distinct integers select each category. A category selected by
  // exactly ONE value states its own number — Arabic's dual `_two` means
  // precisely two, so "[مرفقان]" needs no `{{count}}`, exactly as English
  // `_one` needs none. Russian `_one` also covers 21 and 101, so there the
  // same omission is a real bug. Counting, rather than assuming by suffix
  // name, is what tells those two cases apart.
  const selectors = new Map();
  for (let n = 0; n <= 10000; n += 1) {
    const category = rules.select(n);
    reachable.add(category);
    selectors.set(category, (selectors.get(category) ?? 0) + 1);
  }
  const exactCategories = new Set(
    [...selectors.entries()].filter(([, n]) => n === 1).map(([c]) => c),
  );
  // `other` is i18next's mandatory fallback and is required even where integer
  // counts never select it (Russian reaches it only via fractions).
  const expected = declared.filter((s) => reachable.has(s) || s === "other");
  const unreachable = declared.filter(
    (s) => !reachable.has(s) && s !== "other",
  );

  // 1. Non-plural key parity.
  const flatBase = baseKeys.filter((k) => !splitPlural(k));
  const missing = flatBase.filter((k) => !(k in locale));
  if (missing.length) {
    fail(`${missing.length} missing keys, e.g. ${missing.slice(0, 3).join(", ")}`);
  }
  const orphaned = Object.keys(locale).filter(
    (k) => !splitPlural(k) && !(k in base),
  );
  if (orphaned.length) {
    fail(
      `${orphaned.length} keys not in ${BASE}, e.g. ${orphaned.slice(0, 3).join(", ")}`,
    );
  }

  // 2. Plural families carry exactly the locale's CLDR categories.
  let familiesChecked = 0;
  for (const family of baseFamilies) {
    const present = expected.filter((s) => `${family}_${s}` in locale);
    const extra = PLURAL_SUFFIXES.filter(
      (s) => !declared.includes(s) && `${family}_${s}` in locale,
    );
    if (present.length !== expected.length) {
      const absent = expected.filter((s) => !(`${family}_${s}` in locale));
      fail(`plural family ${family} missing form(s): ${absent.join(", ")}`);
    } else {
      familiesChecked += 1;
    }
    if (extra.length) {
      fail(`plural family ${family} has form(s) ${code} never uses: ${extra.join(", ")}`);
    }
  }

  // 3. Placeholder parity.
  //
  //    Compared like-for-like against the SAME plural form where the base has
  //    one. A locale legitimately ADDS `{{count}}` to a form English left bare:
  //    English `_one` means exactly 1 so "[attachment]" reads fine, but Russian
  //    `_one` also covers 21 and 101, where a bare noun is wrong. So anything
  //    the family uses somewhere is allowed; only a placeholder that appears
  //    nowhere in the family is an error, as is dropping one this same form had.
  let placeholderProblems = 0;
  for (const [key, value] of Object.entries(locale)) {
    const plural = splitPlural(key);
    const sameForm = plural ? key : key;
    const baseKey = plural
      ? [sameForm, `${plural.family}_other`, `${plural.family}_one`].find(
          (k) => k in base,
        )
      : key;
    if (!baseKey || !(baseKey in base)) {
      continue;
    }
    const want = placeholdersOf(base[baseKey]);
    const got = placeholdersOf(value);
    // Everything this family uses anywhere in the base catalogue.
    const familyAllowed = plural
      ? new Set(
          PLURAL_SUFFIXES.flatMap((s) =>
            placeholdersOf(base[`${plural.family}_${s}`]),
          ),
        )
      : new Set(want);
    const unknown = got.filter((p) => !familyAllowed.has(p));
    const exact = plural && exactCategories.has(plural.suffix);
    const dropped = want.filter(
      (p) => !got.includes(p) && !(exact && p === "count"),
    );
    // A form covering MORE than one number must show that number. English
    // `_one` means 1 and reads fine as "[attachment]", so comparing against it
    // proves nothing — Russian `_one` also covers 21 and 101, where the bare
    // noun is simply wrong. This is a positive requirement on the locale, not
    // a diff against the base.
    if (
      plural &&
      !exact &&
      familyAllowed.has("count") &&
      !got.includes("count")
    ) {
      placeholderProblems += 1;
      if (placeholderProblems <= 5) {
        fail(
          `${key}: ${code} '${plural.suffix}' covers more than one number, so it must show {{count}}`,
        );
      }
      continue;
    }
    if (unknown.length || dropped.length) {
      placeholderProblems += 1;
      if (placeholderProblems <= 5) {
        fail(
          `${key}: placeholders differ (base [${want}], locale [${got}])`,
        );
      }
    }
  }
  if (placeholderProblems > 5) {
    fail(`…and ${placeholderProblems - 5} further placeholder mismatches`);
  }

  // 4. Empty values.
  const empty = Object.entries(locale).filter(
    ([, v]) => typeof v === "string" && v.trim() === "",
  );
  if (empty.length) {
    fail(`${empty.length} empty values, e.g. ${empty.slice(0, 3).map(([k]) => k).join(", ")}`);
  }

  console.log(
    `  ${Object.keys(locale).length} keys · ${familiesChecked}/${baseFamilies.size} plural families complete · expects [${expected.join(", ")}]`,
  );
}

// 5. Call sites resolve in the base catalogue.
function sourceFiles(dir, acc = []) {
  for (const entry of readdirSync(dir)) {
    const path = join(dir, entry);
    if (statSync(path).isDirectory()) {
      if (entry !== "locales" && entry !== "__mocks__") {
        sourceFiles(path, acc);
      }
    } else if (/\.(ts|tsx)$/.test(entry)) {
      acc.push(path);
    }
  }
  return acc;
}

/** Does `key` (already namespaced) resolve, counting plural members? */
function resolves(key) {
  return key in base || PLURAL_SUFFIXES.some((s) => `${key}_${s}` in base);
}

console.log("\ncall sites:");
const unresolved = new Set();
let callSites = 0;
for (const file of sourceFiles(SRC_DIR)) {
  const text = readFileSync(file, "utf8");
  const where = file.replace(ROOT + "/", "");

  // Most components scope themselves with `useTranslation("settings")` and
  // then call `t("section.title")` unprefixed. Resolving those is the whole
  // point — they are the large majority of call sites, and a bare regex for
  // `ns:key` would silently check almost nothing.
  const declared = [];
  for (const match of text.matchAll(/useTranslation\(\s*(\[[^\]]*\]|[^)]*)\)/g)) {
    for (const ns of match[1].matchAll(/["'`]([\w.-]+)["'`]/g)) {
      declared.push(ns[1]);
    }
  }
  const namespaces = declared.length ? declared : ["common"];

  const check = (raw) => {
    callSites += 1;
    const candidates = raw.includes(":")
      ? [raw]
      : namespaces.map((ns) => `${ns}:${raw}`);
    if (!candidates.some(resolves)) {
      unresolved.add(`${raw}  (${where})`);
    }
  };

  for (const match of text.matchAll(/\bt\(\s*["'`]([\w.-]+(?::[\w.-]+)?)["'`]/g)) {
    check(match[1]);
  }
  for (const match of text.matchAll(
    /i18nKey=\{?["'`]([\w.-]+(?::[\w.-]+)?)["'`]/g,
  )) {
    check(match[1]);
  }
}
console.log(`  ${callSites} literal call sites checked`);
for (const key of unresolved) {
  fail(`call site has no ${BASE} entry: ${key}`);
}

console.log(
  failures === 0
    ? "\nOK — all catalogues consistent."
    : `\nFAILED — ${failures} problem(s).`,
);
process.exit(failures === 0 ? 0 : 1);
