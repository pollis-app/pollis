#!/usr/bin/env node
/**
 * Glossary check (#1725): a product term in an `en` value is rendered with
 * the glossary's term in each locale that has a column for it.
 *
 * The glossary is frontend/src/i18n/glossary.json. For every key whose `en`
 * value matches a term's `en` regex (or contains a do-not-translate word),
 * the locale's value for that key must contain one of the locale's accepted
 * renderings. A plural family is compared on its `_other` form.
 *
 * WARN-level: a sentence can legitimately drop a term English repeats, so a
 * miss is reported, never fatal. Structural problems in the glossary itself
 * (a bad regex, an `except` key that does not exist, no terms at all) FAIL.
 *
 * Run through `node scripts/i18n-check.mjs`, or directly:
 *   node scripts/i18n-glossary.mjs [--locale ja] [--ns mobile]   every miss, one per line
 */

import { readFileSync, readdirSync, statSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
export const GLOSSARY_PATH = join(ROOT, "frontend/src/i18n/glossary.json");
export const LOCALES_DIR = join(ROOT, "frontend/src/i18n/locales");
const BASE = "en";
const PLURAL = /^(.*)_(zero|one|two|few|many|other)$/;

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

/** `{ "ns:key": value }` for one locale directory. */
export function loadCatalogue(dir) {
  const out = {};
  for (const file of readdirSync(dir).filter((f) => f.endsWith(".json"))) {
    const ns = file.replace(/\.json$/, "");
    for (const [key, value] of Object.entries(flatten(JSON.parse(readFileSync(join(dir, file), "utf8"))))) {
      out[`${ns}:${key}`] = value;
    }
  }
  return out;
}

export function loadGlossary(path = GLOSSARY_PATH) {
  return JSON.parse(readFileSync(path, "utf8"));
}

/** Compile the glossary into matchers; returns `{ rules, problems }`. */
export function compile(glossary, base) {
  const problems = [];
  const rules = [];
  const familyOf = (key) => key.match(PLURAL)?.[1] ?? key;
  const known = new Set(Object.keys(base).flatMap((k) => [k, familyOf(k)]));
  for (const entry of glossary.doNotTranslate ?? []) {
    const escaped = entry.term.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
    rules.push({ id: `dnt:${entry.term}`, re: new RegExp(`(^|[^\\p{L}])${escaped}($|[^\\p{L}])`, "u"), render: () => [entry.term], except: new Set() });
  }
  for (const term of glossary.terms ?? []) {
    let re;
    try {
      re = new RegExp(term.en, term.flags ?? "");
    } catch (error) {
      problems.push(`glossary term ${term.id}: bad regex (${error.message})`);
      continue;
    }
    for (const key of term.except ?? []) {
      if (!known.has(key)) {
        problems.push(`glossary term ${term.id}: except key ${key} is not in ${BASE}`);
      }
    }
    rules.push({ id: term.id, re, render: (code) => term[code], except: new Set(term.except ?? []) });
  }
  if (rules.length === 0) {
    problems.push("glossary has no terms — the check would pass on nothing");
  }
  return { rules, problems };
}

/** Every glossary miss for `locale` against `base`: `[{ key, term, want, en, got }]`. */
export function misses(rules, base, locale, code) {
  const out = [];
  // Do-not-translate words are checked only where the locale has a glossary column at all.
  const columns = rules.some((r) => !r.id.startsWith("dnt:") && r.render(code));
  if (!columns) {
    return out;
  }
  for (const [key, got] of Object.entries(locale)) {
    if (typeof got !== "string") {
      continue;
    }
    const family = key.match(PLURAL)?.[1];
    const rawEn = family ? base[`${family}_other`] ?? base[`${family}_one`] : base[key];
    if (typeof rawEn !== "string") {
      continue;
    }
    // `{{device}}` is a placeholder name, not the word: match the copy around it.
    const enValue = rawEn.replace(/\{\{[^}]*\}\}/g, " ");
    for (const rule of rules) {
      const want = rule.render(code);
      if (!want || rule.except.has(key) || (family && rule.except.has(family))) {
        continue;
      }
      if (!rule.re.test(enValue)) {
        continue;
      }
      if (!want.some((w) => got.includes(w))) {
        out.push({ key, term: rule.id, want, en: enValue, got });
      }
    }
  }
  return out;
}

function localeCodes(dir = LOCALES_DIR) {
  return readdirSync(dir).filter((e) => e !== BASE && statSync(join(dir, e)).isDirectory());
}

/** i18n-check hook: structural problems fail, misses warn with a per-locale count. */
export function check({ fail, warn = (m) => console.warn(`  ⚠ ${m}`), log = console.log } = {}) {
  const base = loadCatalogue(join(LOCALES_DIR, BASE));
  const { rules, problems } = compile(loadGlossary(), base);
  for (const problem of problems) {
    fail(`${problem} — fix frontend/src/i18n/glossary.json`);
  }
  let checked = 0;
  for (const code of localeCodes()) {
    const covered = rules.filter((r) => !r.id.startsWith("dnt:") && r.render(code)).length;
    if (covered === 0) {
      continue;
    }
    checked += 1;
    const found = misses(rules, base, loadCatalogue(join(LOCALES_DIR, code)), code);
    if (found.length) {
      warn(`${code}: ${found.length} value(s) skip a glossary term, e.g. ${found.slice(0, 3).map((m) => `${m.key} (${m.term})`).join(", ")} — node scripts/i18n-glossary.mjs --locale ${code}`);
    } else {
      log(`  ${code}: every glossary term rendered (${covered} terms)`);
    }
  }
  const columns = (loadGlossary().terms ?? []).some((t) => Object.keys(t).some((k) => /^[a-z]{2,3}(-[A-Z][a-z]{3})?$/.test(k) && k !== BASE && k !== "id"));
  if (!columns) {
    fail("the glossary has no locale column — the glossary check checks nothing");
  }
  if (checked === 0) {
    log("  (no shipped locale has a glossary column yet)");
  }
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const args = process.argv.slice(2);
  const only = args.includes("--locale") ? args[args.indexOf("--locale") + 1] : null;
  const ns = args.includes("--ns") ? args[args.indexOf("--ns") + 1] : null;
  const base = loadCatalogue(join(LOCALES_DIR, BASE));
  const { rules, problems } = compile(loadGlossary(), base);
  for (const p of problems) {
    console.error(`✗ ${p}`);
  }
  let total = 0;
  for (const code of localeCodes().filter((c) => !only || c === only)) {
    for (const m of misses(rules, base, loadCatalogue(join(LOCALES_DIR, code)), code)) {
      if (ns && !m.key.startsWith(`${ns}:`)) {
        continue;
      }
      total += 1;
      console.log(`${code} ${m.key} [${m.term} → ${m.want.join(" | ")}]\n   en: ${m.en}\n   ${code}: ${m.got}`);
    }
  }
  console.log(`${total} miss(es)`);
  process.exit(problems.length ? 1 : 0);
}
