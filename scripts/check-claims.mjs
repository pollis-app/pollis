#!/usr/bin/env node
/**
 * check-claims.mjs — every audited public claim is still anchored (#1723).
 *
 * `scripts/claims.json` records each falsifiable sentence our public material
 * makes about how Pollis works (`locator.quote`, in `doc`), the code that makes
 * it true (`anchors`: paths or globs), and the commit it was last verified at.
 * `scripts/claims-impact.mjs` uses the anchors to list the claims a change could
 * break; this checker keeps the ledger from rotting underneath that:
 *
 *   - the file is schema-valid (ids unique, kinds/statuses known, `fact` names a
 *     real entry in scripts/facts.json, `evidence` points at a real line);
 *   - every anchor path or glob matches at least one file — code that a claim
 *     relied on was deleted or moved, so the claim must be re-anchored and
 *     re-verified, not left pointing at nothing;
 *   - every `locator.quote` still appears in its doc (matched with tags,
 *     entities, `**` and whitespace normalised, as check-facts.mjs does) — a
 *     reworded claim is a new claim and must be re-verified;
 *   - every doc in `audited_docs` has at least one claim, and every claim's doc
 *     is listed there (the audit's coverage cannot silently shrink).
 * A ledger with no claims fails: an empty file must not be a silent pass.
 *
 * Run by hand, never in CI: /sweep-claims and the pre-release sweep run it
 * (a sweep check reports drift; it does not gate merges).
 *
 * `status: "disputed"` claims (a legal or policy page that disagrees with code,
 * #1720) are anchored like any other; they print as warnings so they stay visible.
 *
 * Usage:
 *   node scripts/check-claims.mjs                      the repo
 *   --root <dir> / --claims <file>                     check another tree (the self-test)
 *   --list-files                                       print every doc the ledger reads
 */

import { readFileSync, existsSync, readdirSync, statSync } from "node:fs";
import { join, resolve, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { normalize, globToRe } from "./lib/doc-text.mjs";

const args = process.argv.slice(2);
function arg(name) {
  const i = args.indexOf(name);
  if (i === -1) {
    return undefined;
  }
  return args[i + 1];
}

const ROOT = resolve(arg("--root") ?? join(dirname(fileURLToPath(import.meta.url)), ".."));
const CLAIMS = resolve(arg("--claims") ?? join(ROOT, "scripts/claims.json"));
const CLAIMS_REL = CLAIMS.startsWith(ROOT + "/") ? CLAIMS.slice(ROOT.length + 1) : CLAIMS;
const GHA = process.env.GITHUB_ACTIONS === "true";

const KINDS = ["security", "behaviour", "process", "number", "cli"];
const STATUSES = ["verified", "unverifiable", "disputed"];
const ID_RE = /^[a-z0-9]+(?:-[a-z0-9]+)*$/;
const SHA_RE = /^[0-9a-f]{7,40}$/;
const SKIP_DIRS = new Set([".git", "node_modules", "target", ".venv", "Pods", "build", "dist", ".expo"]);

const errors = [];
const warnings = [];
let claimsRaw = "";

function lineOfId(id) {
  const i = claimsRaw.indexOf(`"id": "${id}"`);
  if (i === -1) {
    return CLAIMS_REL;
  }
  return `${CLAIMS_REL}:${claimsRaw.slice(0, i).split("\n").length}`;
}

function fail(id, msg, hint) {
  const where = id ? lineOfId(id) : CLAIMS_REL;
  const line = `${where}: [${id ?? "claims"}] ${msg}${hint ? ` — ${hint}` : ""}`;
  errors.push(line);
  if (GHA) {
    const [file, ln] = where.split(":");
    console.log(`::error file=${file}${ln ? `,line=${ln}` : ""}::[${id ?? "claims"}] ${msg}`);
  }
}

const cache = new Map();
function readRepo(rel) {
  if (!cache.has(rel)) {
    const p = join(ROOT, rel);
    cache.set(rel, existsSync(p) && statSync(p).isFile() ? readFileSync(p, "utf8") : null);
  }
  return cache.get(rel);
}

const normCache = new Map();
function docText(rel) {
  if (!normCache.has(rel)) {
    const raw = readRepo(rel);
    normCache.set(rel, raw === null ? null : normalize(raw, /\.html?$/.test(rel)).text);
  }
  return normCache.get(rel);
}

/** A quote as the checker compares it: whitespace collapsed, Markdown `**` dropped. */
function normQuote(q) {
  return q.replace(/\*\*/g, "").replace(/\s+/g, " ").trim();
}

function isGlob(p) {
  return /[*?]/.test(p);
}

/** Files under the glob's static prefix that match it (the first one only, unless `all`). Walks only that prefix. */
function globFiles(glob, all = false) {
  const found = [];
  const re = globToRe(glob);
  const staticPart = glob.slice(0, glob.search(/[*?]/));
  const base = staticPart.includes("/") ? staticPart.slice(0, staticPart.lastIndexOf("/")) : "";
  const start = join(ROOT, base);
  if (!existsSync(start) || !statSync(start).isDirectory()) {
    return found;
  }
  const stack = [base];
  while (stack.length) {
    const dir = stack.pop();
    for (const ent of readdirSync(join(ROOT, dir), { withFileTypes: true })) {
      const rel = dir ? `${dir}/${ent.name}` : ent.name;
      if (ent.isDirectory()) {
        if (!SKIP_DIRS.has(ent.name)) {
          stack.push(rel);
        }
      } else if (re.test(rel)) {
        found.push(rel);
        if (!all) {
          return found;
        }
      }
    }
  }
  return found;
}

function anchorExists(a) {
  if (isGlob(a)) {
    return globFiles(a).length > 0;
  }
  const p = join(ROOT, a);
  return existsSync(p);
}

function main() {
  if (!existsSync(CLAIMS)) {
    console.error(`${CLAIMS}: not found.`);
    return 1;
  }
  claimsRaw = readFileSync(CLAIMS, "utf8");
  let ledger;
  try {
    ledger = JSON.parse(claimsRaw);
  } catch (e) {
    console.error(`${CLAIMS_REL}: not valid JSON: ${e.message}`);
    return 1;
  }
  const claims = Array.isArray(ledger.claims) ? ledger.claims : [];
  const audited = Array.isArray(ledger.audited_docs) ? ledger.audited_docs : [];

  if (args.includes("--list-files")) {
    // Everything the ledger reads, so the self-test can copy just that into a scratch tree.
    const files = new Set([...audited, "scripts/facts.json"]);
    for (const c of claims) {
      files.add(c.doc);
      [].concat(c.also_rendered_in ?? []).forEach((f) => files.add(f));
      [].concat(c.evidence ?? []).forEach((e) => files.add(e.replace(/:\d+(-\d+)?$/, "")));
      for (const a of [].concat(c.anchors ?? [])) {
        if (isGlob(a)) {
          globFiles(a, true).forEach((f) => files.add(f));
        } else if (existsSync(join(ROOT, a)) && statSync(join(ROOT, a)).isFile()) {
          files.add(a);
        } else if (existsSync(join(ROOT, a))) {
          globFiles(`${a.replace(/\/+$/, "")}/**`, true).forEach((f) => files.add(f));
        }
      }
    }
    for (const f of [...files].filter((f) => existsSync(join(ROOT, f))).sort()) {
      console.log(f);
    }
    return 0;
  }
  if (claims.length === 0) {
    console.error(`${CLAIMS_REL}: no claims declared — refusing to pass on an empty ledger.`);
    return 1;
  }

  let facts = {};
  const factsRaw = readRepo("scripts/facts.json");
  if (factsRaw !== null) {
    facts = JSON.parse(factsRaw).facts ?? {};
  }

  const seen = new Set();
  const perDoc = new Map();
  let anchorsChecked = 0;
  let quotesChecked = 0;
  let disputed = 0;

  for (const c of claims) {
    const id = typeof c.id === "string" ? c.id : undefined;
    if (!id || !ID_RE.test(id)) {
      fail(id, `id ${JSON.stringify(c.id)} is missing or not kebab-case`, "ids are lowercase words joined by hyphens");
      continue;
    }
    if (seen.has(id)) {
      fail(id, "duplicate id", "every claim id is unique");
    }
    seen.add(id);

    for (const field of ["doc", "claim", "verified_sha"]) {
      if (typeof c[field] !== "string" || !c[field].trim()) {
        fail(id, `missing ${field}`);
      }
    }
    if (!KINDS.includes(c.kind)) {
      fail(id, `kind ${JSON.stringify(c.kind)} is not one of ${KINDS.join("|")}`);
    }
    if (!STATUSES.includes(c.status)) {
      fail(id, `status ${JSON.stringify(c.status)} is not one of ${STATUSES.join("|")}`);
    }
    if (c.status === "unverifiable" || c.status === "disputed") {
      if (typeof c.note !== "string" || !c.note.trim()) {
        fail(id, `status ${c.status} needs a note saying why`, "add \"note\": the reason, or the evidence that would settle it");
      }
    }
    if (c.status === "disputed") {
      disputed++;
      warnings.push(`${lineOfId(id)}: [${id}] disputed: ${c.note ?? ""}`);
    }
    if (typeof c.verified_sha === "string" && !SHA_RE.test(c.verified_sha)) {
      fail(id, `verified_sha ${JSON.stringify(c.verified_sha)} is not a commit sha`);
    }
    if (c.fact !== undefined && !(c.fact in facts)) {
      fail(id, `fact ${JSON.stringify(c.fact)} is not in scripts/facts.json`, "use an existing fact id or drop the field");
    }

    if (typeof c.doc === "string") {
      perDoc.set(c.doc, (perDoc.get(c.doc) ?? 0) + 1);
      if (!audited.includes(c.doc)) {
        fail(id, `doc ${c.doc} is not in audited_docs`, "add it to audited_docs, or fix the doc path");
      }
      const text = docText(c.doc);
      const quote = c.locator?.quote;
      if (text === null) {
        fail(id, `doc ${c.doc} does not exist`, "the page moved or was deleted: re-point the claim, or remove it if the claim went with it");
      } else if (typeof quote !== "string" || !quote.trim()) {
        fail(id, "locator.quote is missing");
      } else {
        quotesChecked++;
        if (!text.includes(normQuote(quote))) {
          fail(id, `quote no longer appears in ${c.doc}: "${quote}"`,
            "the copy was reworded: re-verify the claim against the code (/sweep-claims), then update locator.quote and verified_sha");
        }
      }
    }

    for (const r of [].concat(c.also_rendered_in ?? [])) {
      if (readRepo(r) === null) {
        fail(id, `also_rendered_in ${r} does not exist`, "fix the path or drop it");
      }
    }

    const anchors = [].concat(c.anchors ?? []);
    if (anchors.length === 0) {
      fail(id, "has no anchors", "every claim names the code that makes it true");
    }
    for (const a of anchors) {
      anchorsChecked++;
      if (typeof a !== "string" || a.startsWith("/") || a.includes("..")) {
        fail(id, `anchor ${JSON.stringify(a)} is not a repo-relative path or glob`);
        continue;
      }
      if (!anchorExists(a)) {
        fail(id, `anchor ${a} matches no file`,
          "the code this claim relied on moved or was deleted: re-verify the claim and re-anchor it to the code that now makes it true");
      }
    }

    for (const e of [].concat(c.evidence ?? [])) {
      const m = /^(.+?)(?::(\d+)(?:-(\d+))?)?$/.exec(e);
      const raw = m ? readRepo(m[1]) : null;
      if (raw === null) {
        fail(id, `evidence ${e} names a file that does not exist`, "re-verify and repoint the evidence");
        continue;
      }
      const lines = raw.split("\n").length;
      if (m[2] && Number(m[2]) > lines) {
        fail(id, `evidence ${e} is past the end of the file (${lines} lines)`, "re-verify and repoint the evidence");
      }
    }
  }

  for (const d of audited) {
    if (readRepo(d) === null) {
      fail(undefined, `audited doc ${d} does not exist`, "remove it from audited_docs or fix the path");
    } else if (!perDoc.get(d)) {
      fail(undefined, `audited doc ${d} has no claims`, "an audited doc with nothing recorded means the audit coverage shrank");
    }
  }

  for (const w of warnings) {
    console.warn(`  ⚠ ${w}`);
  }
  if (errors.length) {
    console.error(`\nClaims ledger: ${errors.length} problem(s). A public claim lost its anchor or its wording:\n`);
    for (const e of errors) {
      console.error(`  ✗ ${e}`);
    }
    console.error("\nSee scripts/claims.json and .codesight/wiki/sweeps.md → Claims.");
    return 1;
  }
  console.log(`Claims OK — ${claims.length} claim(s) in ${perDoc.size} doc(s), ${quotesChecked} quote(s) and ${anchorsChecked} anchor(s) checked` +
    (disputed ? `, ${disputed} disputed` : "") + ".");
  return 0;
}

process.exit(main());
