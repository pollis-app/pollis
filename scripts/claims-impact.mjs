#!/usr/bin/env node
/**
 * claims-impact.mjs — which public claims a range of commits could have broken (#1723).
 *
 * Every claim in scripts/claims.json names the code that makes it true
 * (`anchors`: repo paths or globs). This lists the claims whose anchors were
 * touched in `git diff <since>..<head>` (renames count on both sides), grouped
 * by the doc that makes the claim, so a re-check reads only those claims
 * instead of re-auditing every page. It reports; it never fails on findings.
 *
 * Usage:
 *   node scripts/claims-impact.mjs --since <ref> [--head <ref>]   grouped list
 *   node scripts/claims-impact.mjs --ledger                       since scripts/sweep-ledger.json → claims.sha
 *   --summary    one line: "N public claims anchored to changed code (…)" (release-impact.sh)
 *   --json       machine-readable: { since, head, changed_files, claims: [{id, doc, anchors_hit, files}] }
 */

import { readFileSync, existsSync } from "node:fs";
import { join, dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { execFileSync } from "node:child_process";
import { globToRe } from "./lib/doc-text.mjs";

const args = process.argv.slice(2);
function arg(name) {
  const i = args.indexOf(name);
  if (i === -1) {
    return undefined;
  }
  return args[i + 1];
}

const ROOT = resolve(join(dirname(fileURLToPath(import.meta.url)), ".."));
const git = (...a) => execFileSync("git", a, { cwd: ROOT, encoding: "utf8", maxBuffer: 1 << 28 });

let since = arg("--since");
const head = arg("--head") ?? "HEAD";
if (args.includes("--ledger")) {
  const ledgerPath = join(ROOT, "scripts/sweep-ledger.json");
  if (!existsSync(ledgerPath)) {
    console.error("scripts/sweep-ledger.json not found");
    process.exit(2);
  }
  since = JSON.parse(readFileSync(ledgerPath, "utf8")).claims?.sha;
  if (!since) {
    console.error("scripts/sweep-ledger.json has no claims.sha");
    process.exit(2);
  }
}
if (!since) {
  console.error("usage: claims-impact.mjs --since <ref> [--head <ref>] [--summary|--json]  (or --ledger)");
  process.exit(2);
}

let sinceSha;
let headSha;
try {
  sinceSha = git("rev-parse", "--verify", `${since}^{commit}`).trim();
  headSha = git("rev-parse", "--verify", `${head}^{commit}`).trim();
} catch {
  console.error(`cannot resolve ${since} or ${head} (shallow clone? fetch more history)`);
  process.exit(2);
}

// --name-status -M: a rename lists the old and the new path, and both count as changed.
const changed = new Set();
for (const line of git("diff", "--name-status", "-M", `${sinceSha}..${headSha}`).split("\n")) {
  const parts = line.split("\t").slice(1);
  for (const p of parts) {
    if (p) {
      changed.add(p);
    }
  }
}

const ledger = JSON.parse(readFileSync(join(ROOT, "scripts/claims.json"), "utf8"));
const matchers = new Map();
function matcher(anchor) {
  if (!matchers.has(anchor)) {
    if (/[*?]/.test(anchor)) {
      const re = globToRe(anchor);
      matchers.set(anchor, (f) => re.test(f));
    } else {
      const dir = anchor.replace(/\/+$/, "") + "/";
      matchers.set(anchor, (f) => f === anchor || f.startsWith(dir));
    }
  }
  return matchers.get(anchor);
}

const files = [...changed];
const hits = [];
for (const c of ledger.claims ?? []) {
  const anchorsHit = [];
  const hitFiles = new Set();
  for (const a of c.anchors ?? []) {
    const m = matcher(a);
    const f = files.filter(m);
    if (f.length) {
      anchorsHit.push(a);
      f.forEach((x) => hitFiles.add(x));
    }
  }
  if (anchorsHit.length) {
    hits.push({ id: c.id, doc: c.doc, claim: c.claim, status: c.status, anchors_hit: anchorsHit, files: [...hitFiles].sort() });
  }
}

const range = `${sinceSha.slice(0, 8)}..${headSha.slice(0, 8)}`;
if (args.includes("--json")) {
  console.log(JSON.stringify({ since: sinceSha, head: headSha, changed_files: files.length, claims: hits }, null, 2));
  process.exit(0);
}

const docs = new Set(hits.map((h) => h.doc));
if (args.includes("--summary")) {
  console.log(`${hits.length} public claim(s) anchored to changed code (${range}, ${docs.size} doc(s))` +
    (hits.length ? " → /sweep-claims" : ""));
  process.exit(0);
}

console.log(`Claims anchored to code changed in ${range} (${files.length} file(s) changed): ${hits.length} claim(s) in ${docs.size} doc(s)`);
const byDoc = new Map();
for (const h of hits) {
  if (!byDoc.has(h.doc)) {
    byDoc.set(h.doc, []);
  }
  byDoc.get(h.doc).push(h);
}
for (const [doc, list] of [...byDoc].sort(([a], [b]) => a.localeCompare(b))) {
  console.log(`\n${doc}`);
  for (const h of list) {
    const shown = h.files.slice(0, 3).join(", ") + (h.files.length > 3 ? `, +${h.files.length - 3}` : "");
    console.log(`  ${h.id}  ${h.claim}`);
    console.log(`      changed: ${shown}`);
  }
}
