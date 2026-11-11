#!/usr/bin/env node
/**
 * check-facts.mjs — every public number or policy value agrees with the code
 * or config that makes it true (#1722).
 *
 * `scripts/facts.json` declares each fact ONCE: where its value comes from
 * (`value_from`: a constant, a wrangler var, CLAUDE.md) and every place we
 * state it (`asserted_in`). Those values change rarely, and when one does,
 * nobody greps the website for it: /learn said "two kinds of loss" for weeks
 * after #720 made it three.
 *
 * An assertion fails when
 *   - its pattern is found but states a different value (the copy is stale), or
 *   - its pattern is not found at all (the section moved or was reworded, so
 *     the fact is no longer anchored and would drift silently).
 * A `value_from` that extracts nothing fails too, as does a registry with no
 * facts: an extraction regex that rots must not become a silent pass.
 *
 * `status: "disputed"` (on a fact, or on one assertion) downgrades its problems
 * to warnings that print both values. It is for a legal or policy page that
 * disagrees with code (#1720 "Policy vs code"): neither side is automatically
 * right, so the disagreement is recorded, not fixed by CI.
 *
 * Modes:
 *   node scripts/check-facts.mjs                      repo mode
 *   node scripts/check-facts.mjs --deployed <site>    website/** assertions are checked
 *                                                     against the DEPLOYED page instead
 *                                                     (after a website deploy)
 *   --root <dir> / --facts <file>                     check another tree (the self-test)
 *   --list-files                                      print every file the registry reads
 *
 * Not CI: run by hand before a release that affects every app
 * (.claude/skills/pre-release-sweeps/SKILL.md).
 */

import { readFileSync, existsSync } from "node:fs";
import { join, resolve, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { normalize, lineOf } from "./lib/doc-text.mjs";

const args = process.argv.slice(2);
function arg(name) {
  const i = args.indexOf(name);
  if (i === -1) {
    return undefined;
  }
  return args[i + 1];
}

const ROOT = resolve(arg("--root") ?? join(dirname(fileURLToPath(import.meta.url)), ".."));
const FACTS = resolve(arg("--facts") ?? join(ROOT, "scripts/facts.json"));
const SITE = arg("--deployed")?.replace(/\/+$/, "");
const GHA = process.env.GITHUB_ACTIONS === "true";

const errors = [];
const warnings = [];

function report(list, kind, where, factId, msg, hint) {
  const line = `${where}: [${factId}] ${msg}${hint ? ` — ${hint}` : ""}`;
  list.push(line);
  if (GHA) {
    const m = /^([^:]+):(\d+)$/.exec(where);
    const loc = m ? ` file=${m[1]},line=${m[2]}` : "";
    console.log(`::${kind}${loc}::[${factId}] ${msg}`);
  }
}

// ── Numbers in words ─────────────────────────────────────────────────────────

const ONES = ["zero", "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten",
  "eleven", "twelve", "thirteen", "fourteen", "fifteen", "sixteen", "seventeen", "eighteen", "nineteen"];
const TENS = ["", "", "twenty", "thirty", "forty", "fifty", "sixty", "seventy", "eighty", "ninety"];
const WORD_TO_NUM = new Map();
function words(n) {
  if (n < 20) {
    return ONES[n];
  }
  if (n < 100) {
    return TENS[Math.floor(n / 10)] + (n % 10 ? `-${ONES[n % 10]}` : "");
  }
  if (n === 100) {
    return "a hundred";
  }
  return null;
}
for (let n = 0; n <= 100; n++) {
  WORD_TO_NUM.set(words(n), n);
}
WORD_TO_NUM.set("hundred", 100);

function escapeRe(s) {
  return s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

/** Every way the text may write `value`: 1000 / 1,000, and twelve / Twelve where `words` allows. */
function valueForms(value, fact) {
  if (typeof value === "string") {
    return [escapeRe(value)];
  }
  const forms = [String(value)];
  if (Number.isInteger(value) && value >= 1000) {
    forms.push(value.toLocaleString("en-US"));
  }
  if (fact.words !== false && Number.isInteger(value)) {
    const w = words(value);
    if (w) {
      forms.push(w, w[0].toUpperCase() + w.slice(1));
    }
  }
  return forms.map(escapeRe);
}

/** A capture that matches whatever value the text states in the same slot, right or wrong. */
function anyValue(value) {
  if (typeof value === "string") {
    return "([A-Za-z0-9][A-Za-z0-9_.-]*)";
  }
  const w = [...WORD_TO_NUM.keys()].sort((a, b) => b.length - a.length).map(escapeRe).join("|");
  const cap = w.replace(/(^|\|)([a-z])/g, (_, p, c) => p + c.toUpperCase());
  return `(\\d[\\d,]*(?:\\.\\d+)?|(?<![A-Za-z])(?:${w}|${cap})(?![a-z]))`;
}

function parseStated(s, value) {
  if (typeof value === "string") {
    return s;
  }
  const lower = s.toLowerCase();
  if (WORD_TO_NUM.has(lower)) {
    return WORD_TO_NUM.get(lower);
  }
  return Number(s.replace(/,/g, ""));
}

// ── Reading text ─────────────────────────────────────────────────────────────

const fileCache = new Map();
function readRepo(rel) {
  if (!fileCache.has(rel)) {
    const p = join(ROOT, rel);
    fileCache.set(rel, existsSync(p) ? readFileSync(p, "utf8") : null);
  }
  return fileCache.get(rel);
}

// ── Values ───────────────────────────────────────────────────────────────────

/** `15 * 60`, `500 * 1024 * 1024`, `10_000`: constants are often written as arithmetic. */
function evalNumber(s) {
  const t = s.replace(/_/g, "").trim();
  if (!/^[\d\s*+\-/().]+$/.test(t)) {
    return null;
  }
  const tokens = t.match(/\d+(?:\.\d+)?|[*+\-/()]/g);
  let pos = 0;
  const expr = () => {
    let v = term();
    while (tokens[pos] === "+" || tokens[pos] === "-") {
      const op = tokens[pos++];
      v = op === "+" ? v + term() : v - term();
    }
    return v;
  };
  const term = () => {
    let v = factor();
    while (tokens[pos] === "*" || tokens[pos] === "/") {
      const op = tokens[pos++];
      v = op === "*" ? v * factor() : v / factor();
    }
    return v;
  };
  const factor = () => {
    const tok = tokens[pos++];
    if (tok === "(") {
      const v = expr();
      pos++;
      return v;
    }
    return Number(tok);
  };
  const v = expr();
  return pos === tokens.length && Number.isFinite(v) ? v : null;
}

function readValue(factId, src) {
  const where = src.file;
  const raw = readRepo(src.file);
  if (raw === null) {
    report(errors, "error", where, factId, `value_from file ${src.file} does not exist`, "update value_from.file in scripts/facts.json");
    return undefined;
  }
  const re = new RegExp(src.pattern, "m");
  const m = re.exec(raw);
  if (!m || m[1] === undefined) {
    report(errors, "error", where, factId, `value_from pattern /${src.pattern}/ matched nothing`,
      "the code moved or was renamed; repoint value_from in scripts/facts.json");
    return undefined;
  }
  let v = m[1];
  if (src.map) {
    if (!(v in src.map)) {
      report(errors, "error", `${where}:${lineOf(raw, m.index)}`, factId, `value_from read "${v}", which value_from.map does not name`,
        "add it to the map in scripts/facts.json");
      return undefined;
    }
    return src.map[v];
  }
  if (src.type === "string") {
    return v;
  }
  if (src.measure === "length") {
    return [...v].length;
  }
  if (src.count) {
    return [...v.matchAll(new RegExp(src.count, "g"))].length;
  }
  let n = WORD_TO_NUM.has(v.toLowerCase()) ? WORD_TO_NUM.get(v.toLowerCase()) : evalNumber(v);
  if (n === null) {
    report(errors, "error", `${where}:${lineOf(raw, m.index)}`, factId, `value_from read "${v}", which is not a number`,
      'set "type": "string" or fix the pattern');
    return undefined;
  }
  if (src.divide) {
    n /= src.divide;
  }
  if (src.multiply) {
    n *= src.multiply;
  }
  return n;
}

// ── Assertions ───────────────────────────────────────────────────────────────

async function fetchPage(rel) {
  let path = rel.replace(/^website\//, "").replace(/\.html$/, "");
  if (path === "index") {
    path = "";
  }
  const url = `${SITE}/${path}`;
  const res = await fetch(url, { redirect: "follow" });
  if (!res.ok) {
    throw new Error(`${url} returned HTTP ${res.status}`);
  }
  const body = await res.text();
  if (!body.trim()) {
    throw new Error(`${url} returned an empty body`);
  }
  return { url, body };
}

const pageCache = new Map();

async function checkAssertion(factId, fact, value, a, disputed) {
  const sink = disputed ? warnings : errors;
  const kind = disputed ? "warning" : "error";
  if (!a.pattern.includes("{value}")) {
    report(errors, "error", a.file, factId, `assertion pattern /${a.pattern}/ has no {value}`, "an assertion must state the value it checks");
    return;
  }
  let raw;
  let label;
  if (SITE) {
    if (!pageCache.has(a.file)) {
      pageCache.set(a.file, fetchPage(a.file).catch((e) => ({ error: e.message })));
    }
    const page = await pageCache.get(a.file);
    if (page.error) {
      report(errors, "error", a.file, factId, page.error, "check the site, or the redirect");
      return;
    }
    raw = page.body;
    label = page.url;
  } else {
    raw = readRepo(a.file);
    label = a.file;
    if (raw === null) {
      report(errors, "error", a.file, factId, `asserted_in file ${a.file} does not exist`, "update asserted_in in scripts/facts.json");
      return;
    }
  }
  const norm = normalize(raw, a.file.endsWith(".html") || a.file.endsWith(".htm"));
  const where = (idx) => (SITE ? label : `${label}:${lineOf(raw, norm.map[idx] ?? 0)}`);
  const flags = a.flags ?? "";
  const right = new RegExp(a.pattern.split("{value}").join(`(?:${valueForms(value, fact).join("|")})`), flags);
  const any = new RegExp(a.pattern.split("{value}").join(anyValue(value)), `g${flags}`);
  const stated = [...norm.text.matchAll(any)];
  if (stated.length === 0) {
    if (right.test(norm.text)) {
      return;
    }
    report(sink, kind, label, factId, `assertion /${a.pattern}/ not found — the text moved or was reworded, so the fact is unanchored`,
      SITE
        ? `the deployed page does not say what ${a.file} on main says; deploy the site (website-deploy.yml)`
        : `re-anchor the pattern in scripts/facts.json (it must still state ${fmt(value, fact)})`);
    return;
  }
  for (const m of stated) {
    const got = m.slice(1).find((g) => g !== undefined);
    const n = parseStated(got, value);
    if (n !== value && !right.test(m[0])) {
      report(sink, kind, where(m.index), factId, `states ${JSON.stringify(got)} but ${fmt(value, fact)} is the truth (${sourceLabel(fact)})`,
        disputed
          ? `disputed: ${a.note ?? fact.note ?? "needs a decision"}`
          : SITE
            ? "the deployed page is stale or wrong; fix the copy on main and deploy the site"
            : "fix the copy, or the code if the copy is the intended behaviour");
    } else if (disputed) {
      report(warnings, "warning", where(m.index), factId, "marked disputed but now agrees with the code", "clear `status: \"disputed\"` in scripts/facts.json");
    }
  }
}

function fmt(value, fact) {
  return fact.unit ? `${value} ${fact.unit}` : JSON.stringify(value);
}

function sourceLabel(fact) {
  return [].concat(fact.value_from).map((s) => s.file).join(" + ");
}

// ── Main ─────────────────────────────────────────────────────────────────────

async function main() {
  const registry = JSON.parse(readFileSync(FACTS, "utf8"));
  const facts = Object.entries(registry.facts ?? {});
  const files = new Set();
  for (const [, f] of facts) {
    for (const s of [].concat(f.value_from ?? [])) {
      files.add(s.file);
    }
    for (const a of f.asserted_in ?? []) {
      files.add(a.file);
    }
  }
  if (args.includes("--list-files")) {
    for (const f of [...files].sort()) {
      console.log(f);
    }
    return 0;
  }
  if (facts.length === 0) {
    console.error(`${FACTS}: no facts declared — refusing to pass on an empty registry.`);
    return 1;
  }

  let assertions = 0;
  let checked = 0;
  for (const [id, fact] of facts) {
    const sources = [].concat(fact.value_from ?? []);
    if (sources.length === 0) {
      report(errors, "error", "scripts/facts.json", id, "has no value_from", "every fact is tied to the code or config that makes it true");
      continue;
    }
    if (!fact.asserted_in?.length) {
      report(errors, "error", "scripts/facts.json", id, "has no asserted_in", "a fact nobody states is not a public fact; remove it");
      continue;
    }
    const values = sources.map((s) => readValue(id, s));
    if (values.some((v) => v === undefined)) {
      continue;
    }
    if (values.some((v) => v !== values[0])) {
      report(errors, "error", sources.map((s) => s.file).join(" + "), id,
        `value_from sources disagree: ${values.map((v, i) => `${sources[i].file} = ${v}`).join(", ")}`,
        "the code default and the deployed config must say the same thing");
      continue;
    }
    const value = values[0];
    for (const a of fact.asserted_in) {
      assertions++;
      if (SITE && !a.file.startsWith("website/")) {
        continue;
      }
      checked++;
      const disputed = (a.status ?? fact.status) === "disputed";
      await checkAssertion(id, fact, value, a, disputed);
    }
  }
  if (!SITE) {
  }

  for (const w of warnings) {
    console.warn(`  ⚠ ${w}`);
  }
  if (checked === 0) {
    console.error("No assertions were checked — the registry or the mode is wrong.");
    return 1;
  }
  if (errors.length) {
    console.error(`\nFacts registry: ${errors.length} problem(s). A public number disagrees with the code that makes it true:\n`);
    for (const e of errors) {
      console.error(`  ✗ ${e}`);
    }
    console.error("\nSee scripts/facts.json.");
    return 1;
  }
  const where = SITE ? `against ${SITE}` : "in the repo";
  console.log(`Facts OK — ${facts.length} fact(s), ${checked} of ${assertions} assertion(s) checked ${where}` +
    (warnings.length ? `, ${warnings.length} disputed warning(s)` : "") + ".");
  return 0;
}

process.exit(await main());
