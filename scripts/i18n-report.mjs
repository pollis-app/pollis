/**
 * `node scripts/i18n-check.mjs --report` (#1724): how far each locale has got, and where every
 * surface outside the catalogues stands.
 *
 *   1. coverage per locale and namespace: units translated / total, where a unit is one `en`
 *      key, or one plural family (counted once, translated when the locale has any of its forms;
 *      i18n-check.mjs fails a family that is started but incomplete);
 *   2. every surface in scripts/i18n-surfaces.json with its decision and status, after
 *      validating the file (fields, values, paths that exist, the surfaces #1724 requires).
 *
 * Exit 1 when the registry is invalid; low coverage is a report, never a failure. Read by
 * scripts/sweep.mjs (dimension `i18n-surfaces`).
 */

import { existsSync, readFileSync, readdirSync, statSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const LOCALES_DIR = join(ROOT, "frontend/src/i18n/locales");
export const SURFACES_PATH = join(ROOT, "scripts/i18n-surfaces.json");
const BASE = "en";

// The surfaces #1724 named; the registry may add more, never drop one.
export const REQUIRED = [
  "ios-permission-strings",
  "android-permission-rationale",
  "push-android",
  "push-ios",
  "tui",
  "core-errors",
  "ds-errors",
  "email-otp",
  "email-change",
  "store-listing",
  "website",
  "learn",
  "whitepaper",
  "docs",
];
const DECISIONS = ["localize", "english-only"];
const STATUSES = ["localized", "partial", "debt"];

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

/** `ns` → set of units (`key`, or `family` for a plural family) a catalogue has. */
function units(code) {
  const out = {};
  const dir = join(LOCALES_DIR, code);
  for (const file of readdirSync(dir).filter((f) => f.endsWith(".json"))) {
    const ns = file.replace(/\.json$/, "");
    const set = new Set();
    for (const key of Object.keys(flatten(JSON.parse(readFileSync(join(dir, file), "utf8"))))) {
      set.add(key.replace(/_(zero|one|two|few|many|other)$/, ""));
    }
    out[ns] = set;
  }
  return out;
}

/** `{ code: { ns: { have, total } } }` against `en`. */
export function coverage() {
  const base = units(BASE);
  const codes = readdirSync(LOCALES_DIR).filter((c) => statSync(join(LOCALES_DIR, c)).isDirectory()).sort();
  const out = {};
  for (const code of codes) {
    const locale = units(code);
    out[code] = {};
    for (const [ns, keys] of Object.entries(base)) {
      const have = [...keys].filter((k) => locale[ns]?.has(k)).length;
      out[code][ns] = { have, total: keys.size };
    }
  }
  return out;
}

/** Problems with the registry, as `file: message` lines; empty when valid. */
export function validateSurfaces(registry, root = ROOT) {
  const problems = [];
  const where = "scripts/i18n-surfaces.json";
  const surfaces = registry?.surfaces ?? {};
  if (!Object.keys(surfaces).length) {
    return [`${where}: empty registry — every surface needs an entry`];
  }
  for (const id of REQUIRED) {
    if (!surfaces[id]) {
      problems.push(`${where}: required surface "${id}" is missing — add it with a decision and a reason`);
    }
  }
  for (const [id, s] of Object.entries(surfaces)) {
    const at = `${where} → ${id}`;
    if (!s.title || !s.reason) {
      problems.push(`${at}: needs a title and a reason`);
    }
    if (!DECISIONS.includes(s.decision)) {
      problems.push(`${at}: decision must be one of ${DECISIONS.join(", ")}`);
    }
    if (s.decision === "localize") {
      if (!STATUSES.includes(s.status)) {
        problems.push(`${at}: a localize surface needs a status: ${STATUSES.join(", ")}`);
      }
      if ((s.status === "debt" || s.status === "partial") && !s.debt) {
        problems.push(`${at}: status ${s.status} needs "debt": what is left and what it needs`);
      }
      if ((s.status === "localized" || s.status === "partial") && !s.checked_by) {
        problems.push(`${at}: a localized surface needs "checked_by": the test or check that keeps it so`);
      }
    } else if (s.status !== undefined) {
      problems.push(`${at}: an english-only surface has no status — the reason is the record`);
    }
    if (!Array.isArray(s.paths) || !s.paths.length) {
      problems.push(`${at}: needs "paths" (the code or content that produces the text)`);
    }
    for (const p of s.paths ?? []) {
      if (!existsSync(join(root, p))) {
        problems.push(`${at}: path ${p} does not exist — it moved or was deleted; fix the entry`);
      }
    }
  }
  return problems;
}

const pct = (have, total) => (total ? `${Math.floor((100 * have) / total)}%` : "—");

/** Print the report; returns the exit code. */
export function report({ log = console.log, error = console.error } = {}) {
  const cov = coverage();
  const codes = Object.keys(cov);
  const namespaces = Object.keys(cov[BASE]).sort();
  log("Locale coverage (units translated / total; a plural family is one unit)\n");
  log(["namespace".padEnd(10), ...codes.map((c) => c.padStart(11))].join(" "));
  for (const ns of namespaces) {
    log([ns.padEnd(10), ...codes.map((c) => `${cov[c][ns].have}/${cov[c][ns].total}`.padStart(11))].join(" "));
  }
  const totals = codes.map((c) => {
    const have = namespaces.reduce((a, ns) => a + cov[c][ns].have, 0);
    const total = namespaces.reduce((a, ns) => a + cov[c][ns].total, 0);
    return `${have}/${total} ${pct(have, total)}`;
  });
  log(["total".padEnd(10), ...totals.map((t) => t.padStart(11))].join(" "));

  let registry;
  try {
    registry = JSON.parse(readFileSync(SURFACES_PATH, "utf8"));
  } catch (e) {
    error(`\n  ✗ scripts/i18n-surfaces.json: ${e.message}`);
    return 1;
  }
  log("\nSurfaces (scripts/i18n-surfaces.json)\n");
  const surfaces = Object.entries(registry.surfaces ?? {});
  const label = (s) => (s.decision === "english-only" ? "english-only" : s.status ?? "?");
  for (const group of ["localized", "partial", "debt", "english-only"]) {
    const rows = surfaces.filter(([, s]) => label(s) === group);
    if (!rows.length) {
      continue;
    }
    log(`${group} (${rows.length})`);
    for (const [id, s] of rows) {
      log(`  ${id.padEnd(30)} ${s.title}`);
      if (s.debt) {
        log(`  ${"".padEnd(30)} ↳ ${s.debt}`);
      }
    }
  }
  const problems = validateSurfaces(registry);
  for (const p of problems) {
    error(`  ✗ ${p}`);
  }
  const counts = ["localized", "partial", "debt", "english-only"].map((g) => `${surfaces.filter(([, s]) => label(s) === g).length} ${g}`);
  log(problems.length ? `\nFAILED — ${problems.length} registry problem(s).` : `\nOK — ${surfaces.length} surfaces: ${counts.join(", ")}.`);
  return problems.length ? 1 : 0;
}
