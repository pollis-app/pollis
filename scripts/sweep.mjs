#!/usr/bin/env node
/**
 * sweep.mjs — the engine behind /pre-release-sweeps (epic #1720, #1728).
 *
 * One entry point for every drift sweep. For each dimension it:
 *   1. runs the dimension's tier-1 checker(s), if the dimension is built, and reports pass/fail;
 *   2. diffs <base>..HEAD, where <base> is --since or the dimension's sha in scripts/sweep-ledger.json,
 *      and maps the changed paths onto what that dimension checks (claims anchors, fact sources,
 *      content registered in scripts/surfaces.json, catalogue keys, feature code) to say what is DUE;
 *   3. reports a dimension whose lane has not landed as "not yet built (#lane)", never as passing.
 * It also checks the surface registry itself (scripts/surfaces.json) in both directions: every
 * public file is registered, and every registered path still exists.
 *
 * Like every drift sweep it runs by hand (/pre-release-sweeps) and never gates a merge.
 *
 * Usage:
 *   node scripts/sweep.mjs [dimension ...]        run the checkers and print the due list
 *     --since <ref>     diff from <ref> for every dimension instead of the ledger's sha
 *     --head <ref>      diff up to <ref> (default HEAD)
 *     --quick           skip slow checkers (the cargo-built CLI parse test); they print SKIPPED
 *     --no-checks       due list only, no checkers
 *     --summary         one line, "sweeps due: …" (what /release-check prints); implies --no-checks
 *     --json            machine-readable
 *     --list            the dimensions, their lanes, checkers and procedures
 *     --bump <dims> [--by <name>] [--at <ref>]   record a finished sweep: set each comma-separated dimension's ledger
 *                       entry to <ref> (default: where HEAD left origin/main, since PRs are squash-merged), today
 *     --root <dir>      run against another tree (the self-test); --list-files prints what the registry check reads
 * Exit status: 1 if a checker or the registry check failed, 2 on bad usage, else 0. --summary always exits 0.
 */

import { existsSync, readFileSync, readdirSync, statSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { execFileSync, spawnSync } from "node:child_process";

const argv = process.argv.slice(2);
const VALUE_FLAGS = new Set(["--since", "--head", "--root", "--bump", "--by", "--at"]);
const opts = {};
const positional = [];
for (let i = 0; i < argv.length; i++) {
  const a = argv[i];
  if (VALUE_FLAGS.has(a)) {
    if (argv[i + 1] === undefined) {
      usage(`${a} needs a value`);
    }
    opts[a] = argv[++i];
  } else if (a.startsWith("--")) {
    opts[a] = true;
  } else {
    positional.push(a);
  }
}
const KNOWN_FLAGS = new Set([...VALUE_FLAGS, "--list-files", "--quick", "--no-checks", "--summary", "--json", "--list", "--help", "-h"]);
for (const k of Object.keys(opts)) {
  if (!KNOWN_FLAGS.has(k)) {
    usage(`unknown flag ${k}`);
  }
}
if (opts["--help"]) {
  const src = readFileSync(fileURLToPath(import.meta.url), "utf8");
  console.log(src.slice(src.indexOf("/**") + 4, src.indexOf("*/")).replace(/^ \* ?/gm, ""));
  process.exit(0);
}

function usage(msg) {
  console.error(`sweep.mjs: ${msg}\nusage: node scripts/sweep.mjs [dimension ...] [--since <ref>] [--quick] [--no-checks] [--summary] [--json] [--list] [--bump <dims>]`);
  process.exit(2);
}

const ROOT = resolve(opts["--root"] ?? join(dirname(fileURLToPath(import.meta.url)), ".."));
const SUMMARY = Boolean(opts["--summary"]);
const RUN_CHECKS = !SUMMARY && !opts["--no-checks"];
const git = (...a) => execFileSync("git", a, { cwd: ROOT, encoding: "utf8", maxBuffer: 1 << 28, stdio: ["ignore", "pipe", "pipe"] });
const read = (p) => readFileSync(join(ROOT, p), "utf8");
const exists = (p) => existsSync(join(ROOT, p));
const readJson = (p) => JSON.parse(read(p));
const LEDGER_PATH = "scripts/sweep-ledger.json";

// ---------------------------------------------------------------------------
// Globs. `**/` matches zero or more directories; `*` and `?` stay inside one segment.
// A pattern without a wildcard matches the path itself and, as a directory, everything under it.
// ---------------------------------------------------------------------------
const globCache = new Map();
function matcher(pattern) {
  if (!globCache.has(pattern)) {
    let fn;
    if (/[*?]/.test(pattern)) {
      let re = "";
      for (let i = 0; i < pattern.length; i++) {
        const c = pattern[i];
        if (c === "*" && pattern[i + 1] === "*") {
          if (pattern[i + 2] === "/") {
            re += "(?:.*/)?";
            i += 2;
          } else {
            re += ".*";
            i++;
          }
        } else if (c === "*") {
          re += "[^/]*";
        } else if (c === "?") {
          re += "[^/]";
        } else {
          re += c.replace(/[.+^${}()|[\]\\]/g, "\\$&");
        }
      }
      const rx = new RegExp(`^${re}$`);
      fn = (f) => rx.test(f);
    } else {
      const dir = pattern.replace(/\/+$/, "") + "/";
      fn = (f) => f === pattern || f.startsWith(dir);
    }
    globCache.set(pattern, fn);
  }
  return globCache.get(pattern);
}
const matchesAny = (patterns, f) => patterns.some((p) => matcher(p)(f));

let trackedCache;
function tracked() {
  if (!trackedCache) {
    trackedCache = git("ls-files", "--cached", "--others", "--exclude-standard").split("\n").filter(Boolean);
  }
  return trackedCache;
}

// ---------------------------------------------------------------------------
// The ledger and the diff since a dimension's last sweep
// ---------------------------------------------------------------------------
const ledger = exists(LEDGER_PATH) ? readJson(LEDGER_PATH) : {};
const headRef = opts["--head"] ?? "HEAD";
let headSha;
try {
  headSha = git("rev-parse", "--verify", `${headRef}^{commit}`).trim();
} catch {
  usage(`cannot resolve --head ${headRef}`);
}
if (opts["--since"]) {
  try {
    git("rev-parse", "--verify", `${opts["--since"]}^{commit}`);
  } catch {
    usage(`cannot resolve --since ${opts["--since"]} (shallow clone? fetch more history)`);
  }
}

const diffCache = new Map();
/** Changed paths in base..head; a rename counts on both sides. */
function changedSince(baseSha) {
  if (!diffCache.has(baseSha)) {
    const files = new Set();
    const added = new Set();
    for (const line of git("diff", "--name-status", "-M", `${baseSha}..${headSha}`).split("\n")) {
      const [status, ...paths] = line.split("\t");
      for (const p of paths) {
        if (p) {
          files.add(p);
        }
      }
      if (status && (status[0] === "A" || status[0] === "R") && paths.length) {
        added.add(paths[paths.length - 1]);
      }
    }
    diffCache.set(baseSha, { files: [...files], added: [...added] });
  }
  return diffCache.get(baseSha);
}

/** The ref a dimension diffs from: --since, else its ledger sha, else null (never swept). */
function baseFor(dim) {
  const ref = opts["--since"] ?? ledger[dim]?.sha;
  if (!ref) {
    return null;
  }
  try {
    const sha = git("rev-parse", "--verify", `${ref}^{commit}`).trim();
    return { ref, sha, short: sha.slice(0, 8), from: opts["--since"] ? "--since" : `ledger ${ledger[dim].date ?? ""}`.trim() };
  } catch {
    return { ref, sha: null, short: ref.slice(0, 8), error: `cannot resolve ${ref} (shallow clone? git fetch --unshallow)` };
  }
}

// ---------------------------------------------------------------------------
// The surface registry (scripts/surfaces.json)
// ---------------------------------------------------------------------------
const SURFACES_PATH = "scripts/surfaces.json";
const surfaces = exists(SURFACES_PATH) ? readJson(SURFACES_PATH) : null;

function contentFiles(item) {
  return [item.source, ...(item.rendered ?? [])].filter(Boolean);
}

/** An item's files minus the ones another item owns as its source (the /learn topics share website/learn.html). */
function ownFiles(id) {
  const others = new Set(Object.entries(surfaces.content).filter(([k]) => k !== id).map(([, it]) => it.source));
  return contentFiles(surfaces.content[id]).filter((p) => !others.has(p));
}

/** Content items checked by `dim` whose files changed in `files`. */
function changedContent(dim, files) {
  if (!surfaces) {
    return [];
  }
  const out = [];
  for (const [id, item] of Object.entries(surfaces.content ?? {})) {
    if (!(item.checked_by ?? []).includes(dim)) {
      continue;
    }
    const pats = ownFiles(id);
    if (files.some((f) => matchesAny(pats, f))) {
      out.push(id);
    }
  }
  return out;
}

/** Both-directions registration check. Returns { problems: [string], counts }. */
function checkRegistry() {
  const problems = [];
  if (!surfaces) {
    return { problems: [`${SURFACES_PATH}: missing`], counts: {} };
  }
  const where = (key) => `${SURFACES_PATH} → ${key}`;
  const files = tracked();
  const axes = surfaces.axes ?? {};
  const platforms = new Set(axes.platform ?? []);
  const content = surfaces.content ?? {};
  const stores = surfaces.stores ?? {};
  const internal = surfaces.internal ?? {};
  const globs = surfaces.registration?.globs ?? [];
  const dimIds = new Set(DIMENSIONS.map((d) => d.id));

  if (!Object.keys(content).length) {
    problems.push(`${where("content")}: empty registry. Every public page, doc and listing must be registered.`);
  }
  if (!globs.length) {
    problems.push(`${where("registration.globs")}: empty. Nothing would be checked for registration.`);
  }

  // Registered → exists. Every source/rendered path or glob must match a tracked file.
  for (const [id, item] of Object.entries(content)) {
    if (!item.source) {
      problems.push(`${where(`content.${id}`)}: no source`);
    }
    for (const p of contentFiles(item)) {
      if (!files.some(matcher(p))) {
        problems.push(`${where(`content.${id}`)}: "${p}" matches no tracked file. Moved or deleted? Re-point or remove the entry.`);
      }
    }
    for (const d of item.checked_by ?? []) {
      if (!dimIds.has(d)) {
        problems.push(`${where(`content.${id}.checked_by`)}: unknown dimension "${d}" (node scripts/sweep.mjs --list)`);
      }
    }
    for (const s of item.stores ?? []) {
      if (!stores[s]) {
        problems.push(`${where(`content.${id}.stores`)}: unknown store "${s}"`);
      }
    }
  }
  for (const p of Object.keys(internal)) {
    if (!files.some(matcher(p))) {
      problems.push(`${where(`internal["${p}"]`)}: matches no tracked file. Remove the entry.`);
    }
    if (!internal[p]) {
      problems.push(`${where(`internal["${p}"]`)}: give the reason it is not public content`);
    }
  }

  // Stores: axes.store and stores must name the same set, and each listing must be registered content.
  const axisStores = new Set(axes.store ?? []);
  for (const s of axisStores) {
    if (!stores[s]) {
      problems.push(`${where("stores")}: axes.store lists "${s}" but stores has no entry for it`);
    }
  }
  for (const [s, entry] of Object.entries(stores)) {
    if (!axisStores.has(s)) {
      problems.push(`${where(`stores.${s}`)}: not in axes.store`);
    }
    for (const c of entry.listing ?? []) {
      if (!content[c]) {
        problems.push(`${where(`stores.${s}.listing`)}: "${c}" is not a content id`);
      }
    }
  }

  // Exists → registered. Every tracked file under a registration glob is claimed by content or internal.
  const claimed = [
    ...Object.values(content).flatMap(contentFiles),
    ...Object.keys(internal),
    ...Object.values(surfaces.data_files ?? {}),
  ];
  let scanned = 0;
  for (const f of files) {
    if (!matchesAny(globs, f)) {
      continue;
    }
    scanned++;
    if (!matchesAny(claimed, f)) {
      problems.push(`${f}: public-content path not registered. Add it to ${SURFACES_PATH} → content (with checked_by), or → internal with the reason it is not public.`);
    }
  }
  if (globs.length && scanned === 0) {
    problems.push(`${where("registration.globs")}: matched no tracked file (extraction broke?)`);
  }

  // Cross-check with the other lanes' registries, where the answer is computable.
  const claimsPath = surfaces.data_files?.claims ?? "scripts/claims.json";
  if (exists(claimsPath)) {
    for (const doc of readJson(claimsPath).audited_docs ?? []) {
      const owners = Object.entries(content).filter(([, it]) => matchesAny(contentFiles(it), doc));
      if (!owners.length) {
        problems.push(`${claimsPath}: audited doc ${doc} is not registered content in ${SURFACES_PATH}`);
      } else if (!owners.some(([, it]) => (it.checked_by ?? []).includes("claims"))) {
        problems.push(`${where(`content.${owners[0][0]}.checked_by`)}: ${doc} is in claims.json audited_docs; add "claims"`);
      }
    }
  }
  const factsPath = surfaces.data_files?.facts ?? "scripts/facts.json";
  if (exists(factsPath)) {
    const factFiles = new Set(Object.values(readJson(factsPath).facts ?? {}).flatMap((f) => (f.asserted_in ?? []).map((a) => a.file)));
    for (const [id, item] of Object.entries(content)) {
      const states = ownFiles(id).some((p) => [...factFiles].some(matcher(p)));
      const says = (item.checked_by ?? []).includes("facts");
      if (states && !says) {
        problems.push(`${where(`content.${id}.checked_by`)}: facts.json asserts values in it; add "facts"`);
      }
      if (says && !states) {
        problems.push(`${where(`content.${id}.checked_by`)}: says "facts" but facts.json asserts nothing in it; remove "facts"`);
      }
    }
  }

  // Features: axes must exist; paths must exist.
  for (const [id, feat] of Object.entries(surfaces.features ?? {})) {
    for (const a of feat.varies_on ?? []) {
      if (!(a in axes)) {
        problems.push(`${where(`features.${id}.varies_on`)}: unknown axis "${a}"`);
      }
    }
    for (const p of feat.platforms ?? []) {
      if (!platforms.has(p)) {
        problems.push(`${where(`features.${id}.platforms`)}: unknown platform "${p}"`);
      }
    }
    for (const p of feat.paths ?? []) {
      if (!files.some(matcher(p))) {
        problems.push(`${where(`features.${id}.paths`)}: "${p}" matches no tracked file`);
      }
    }
  }
  return {
    problems,
    counts: { content: Object.keys(content).length, stores: Object.keys(stores).length, internal: Object.keys(internal).length, scanned, features: Object.keys(surfaces.features ?? {}).length },
  };
}

// ---------------------------------------------------------------------------
// Dimension-specific due computations. Each returns { due: [lines], count, unit } given the diff.
// ---------------------------------------------------------------------------
function dueFromContent(dim, files, label = "registered public item(s) it checks changed") {
  const items = changedContent(dim, files);
  return { lines: items.length ? [`${items.length} ${label}: ${listShort(items)}`] : [], count: items.length };
}

function listShort(xs, n = 6) {
  return xs.slice(0, n).join(", ") + (xs.length > n ? `, +${xs.length - n} more` : "");
}

function dueFacts(base, diff) {
  const facts = readJson("scripts/facts.json").facts ?? {};
  const hit = [];
  for (const [id, f] of Object.entries(facts)) {
    const sources = (Array.isArray(f.value_from) ? f.value_from : [f.value_from]).filter(Boolean).map((v) => v.file);
    if (diff.files.some((x) => sources.includes(x))) {
      hit.push(id);
    }
  }
  const lines = [];
  if (hit.length) {
    lines.push(`${hit.length} fact(s) whose source changed (the checker re-proves the copies): ${listShort(hit)}`);
  }
  const content = changedContent("facts", diff.files);
  if (content.length) {
    lines.push(`${content.length} page(s)/doc(s) stating facts changed: look for NEW public numbers to register: ${listShort(content)}`);
  }
  return { lines, count: hit.length + content.length };
}

function dueClaims(base) {
  const r = spawnSync("node", ["scripts/claims-impact.mjs", "--since", base.sha, "--head", headSha, "--json"], { cwd: ROOT, encoding: "utf8", maxBuffer: 1 << 28 });
  if (r.status !== 0) {
    return { lines: [`claims-impact.mjs failed: ${(r.stderr || r.stdout).trim().split("\n")[0]}`], count: 1 };
  }
  const j = JSON.parse(r.stdout);
  const docs = new Set(j.claims.map((c) => c.doc));
  const lines = [];
  if (j.claims.length) {
    lines.push(`${j.claims.length} claim(s) anchored to changed code in ${docs.size} doc(s) → /sweep-claims`);
  }
  return { lines, count: j.claims.length };
}

function dueCli(base, diff) {
  const lines = [];
  const cliCode = diff.files.filter((f) => f.startsWith("verifiable-log-serve/src/"));
  if (cliCode.length) {
    lines.push(`pollis-verify's CLI code changed (${cliCode.length} file(s)): re-run the parse test and the live run`);
  }
  return { lines, count: cliCode.length };
}

const INSTALLER_PATHS = ["website/install.sh", "website/cli-install.sh", "docs/deployments.md"];
function dueInstallers(base, diff) {
  const hit = diff.files.filter((f) => INSTALLER_PATHS.includes(f));
  return { lines: hit.length ? [`installer or its env-var doc changed: ${hit.join(", ")}`] : [], count: hit.length };
}

function dueParity(base) {
  const r = spawnSync("node", ["scripts/feature-parity.mjs", "--since", base.sha, "--json"], { cwd: ROOT, encoding: "utf8", maxBuffer: 1 << 28 });
  if (r.status !== 0) {
    return { lines: [`feature-parity.mjs --since failed: ${(r.stderr || "").trim().split("\n")[0]}`], count: 1 };
  }
  const j = JSON.parse(r.stdout);
  const gaps = [...(j.desktop_only ?? []), ...(j.mobile_only ?? []), ...(j.missing_from_cli ?? [])];
  return { lines: gaps.length ? [`${gaps.length} parity gap(s) new since ${base.short}: ${listShort(gaps)}`] : [], count: gaps.length };
}

const LOCALES = "frontend/src/i18n/locales";
function flatten(o, prefix = "", out = {}) {
  for (const [k, v] of Object.entries(o ?? {})) {
    const p = prefix ? `${prefix}.${k}` : k;
    if (v && typeof v === "object" && !Array.isArray(v)) {
      flatten(v, p, out);
    } else {
      out[p] = v;
    }
  }
  return out;
}
/** Plural members (_one, _few …) collapse to their family: a locale has the key if it has any member. */
const family = (k) => k.replace(/_(zero|one|two|few|many|other)$/, "");
function catalogueKeys(files) {
  const keys = new Set();
  for (const [ns, text] of files) {
    try {
      for (const k of Object.keys(flatten(JSON.parse(text)))) {
        keys.add(`${ns}:${family(k)}`);
      }
    } catch {
      // a catalogue that does not parse is i18n-check.mjs's finding, not ours
    }
  }
  return keys;
}
function dueI18n(base, diff) {
  const nsOf = (p) => p.split("/").pop().replace(/\.json$/, "");
  const enAt = (sha) => {
    let list = [];
    try {
      list = git("ls-tree", "-r", "--name-only", sha, "--", `${LOCALES}/en`).split("\n").filter((p) => p.endsWith(".json"));
    } catch {
      return new Set();
    }
    return catalogueKeys(list.map((p) => [nsOf(p), git("show", `${sha}:${p}`)]));
  };
  const before = enAt(base.sha);
  const after = enAt(headSha);
  const added = [...after].filter((k) => !before.has(k));
  if (!added.length) {
    return { lines: [], count: 0 };
  }
  const missing = [];
  for (const code of readdirSync(join(ROOT, LOCALES)).filter((c) => c !== "en" && statSync(join(ROOT, LOCALES, c)).isDirectory())) {
    const dir = join(LOCALES, code);
    const have = catalogueKeys(readdirSync(join(ROOT, dir)).filter((f) => f.endsWith(".json")).map((f) => [nsOf(f), read(join(dir, f))]));
    const n = added.filter((k) => !have.has(k)).length;
    if (n) {
      missing.push(`${code} ${n}`);
    }
  }
  const lines = [`${added.length} en key(s) added since ${base.short}` + (missing.length ? `; missing in ${missing.length} locale(s): ${missing.join(", ")}` : "; every locale has them")];
  return { lines, count: missing.length ? added.length : 0 };
}

function dueSurfaces(base, diff) {
  const globs = surfaces?.registration?.globs ?? [];
  const added = diff.added.filter((f) => matchesAny(globs, f));
  return {
    lines: added.length ? [`${added.length} public-content file(s) added or renamed: confirm each entry's checked_by and stores: ${listShort(added)}`] : [],
    count: added.length,
  };
}

// ---------------------------------------------------------------------------
// The dimensions. `files` = what must exist for the lane to count as built.
// `checks` = tier-1 commands (argv, run from the repo root); `slow` ones are skipped by --quick.
// `procedure` = the section of .claude/skills/pre-release-sweeps/SKILL.md to follow when due.
// ---------------------------------------------------------------------------
const DIMENSIONS = [
  {
    id: "surfaces", ticket: "#1728", what: "every public page, doc and store is registered in scripts/surfaces.json",
    files: ["scripts/surfaces.json"], internal: "registry", due: dueSurfaces,
  },
  {
    id: "facts", ticket: "#1722", what: "public numbers and policy values match the code",
    files: ["scripts/facts.json", "scripts/check-facts.mjs"], checks: [["node", "scripts/check-facts.mjs"]], due: dueFacts,
  },
  {
    id: "cli", ticket: "#1721", what: "published pollis-verify commands parse and run against the live log",
    files: ["verifiable-log-serve/tests/doc_invocations.rs"],
    checks: [{ argv: ["cargo", "test", "-q", "-p", "verifiable-log-serve", "--test", "doc_invocations", "--", "--include-ignored"], slow: true }],
    manual: "the live run against verify.pollis.com (skill § cli)",
    due: (b, d) => {
      const r = dueCli(b, d);
      const c = dueFromContent("cli", d.files, "page(s)/doc(s) showing pollis-verify commands changed");
      return { lines: [...r.lines, ...c.lines], count: r.count || c.count };
    },
  },
  {
    id: "installers", ticket: "#1721", what: "installer env vars are documented, and only those",
    files: ["scripts/check-install-env.py"], checks: [["python3", "scripts/check-install-env.py"]], due: dueInstallers,
  },
  {
    id: "claims", ticket: "#1723", what: "every audited public claim is anchored and still true",
    files: ["scripts/claims.json", "scripts/check-claims.mjs", "scripts/claims-impact.mjs"],
    checks: [["node", "scripts/check-claims.mjs"], ["node", "scripts/check-learn-sync.mjs"]],
    due: (b, d) => {
      const r = dueClaims(b);
      const c = dueFromContent("claims", d.files, "audited doc(s) changed: read the diff for NEW claims (sweep-claims step 2.4)");
      return { lines: [...r.lines, ...c.lines], count: r.count || c.count };
    },
  },
  {
    id: "feature-parity", ticket: "#1347", what: "desktop, mobile and the CLI call the same core commands",
    files: ["scripts/feature-parity.mjs", "scripts/feature-parity.json"], checks: [{ argv: ["node", "scripts/feature-parity.mjs", "--json"], parity: true }], due: dueParity,
  },
  {
    id: "i18n", ticket: "#1720", what: "catalogue keys exist in every locale, placeholders and plurals intact",
    files: ["scripts/i18n-check.mjs"], checks: [["node", "scripts/i18n-check.mjs"]], due: dueI18n,
  },
  {
    id: "i18n-surfaces", ticket: "#1724", what: "user-facing copy outside the catalogues",
    files: ["scripts/i18n-surfaces.json"],
  },
  {
    id: "ui-inventory", ticket: "#933", what: ".codesight/wiki/ui.md lists every component",
    files: ["scripts/ui-inventory.mjs"], checks: [["node", "scripts/ui-inventory.mjs", "--check"]], mechanical: true,
  },
  {
    id: "ds-config", ticket: "#760", what: "every DS config value reaches the container",
    files: ["scripts/check-ds-config-chain.py"], checks: [["python3", "scripts/check-ds-config-chain.py"]], mechanical: true,
  },
  {
    id: "store-assets", ticket: "#1729", what: "store screenshots and promo images show the current UI",
    files: ["mobile/store/screenshots"],
  },
  {
    id: "duplication", ticket: "#1731", what: "no new copy-paste clones against the committed baseline",
    files: [".claude/skills/reuse-check/SKILL.md"],
  },
  {
    id: "platform-matrix", ticket: "#1728", what: "which platform × skin cells have automated coverage (report only)",
    files: ["scripts/surfaces.json"], internal: "matrix", due: dueMatrix,
  },
];

// ---------------------------------------------------------------------------
// Platform matrix (report only): which platform × skin cells have automated coverage, generated from the
// CI workflows and the e2e suites, and which features (surfaces.json → features) the gaps put at risk.
// ---------------------------------------------------------------------------
const LEVELS = ["none", "build/unit only", "e2e by hand", "e2e in CI (not per PR)", "e2e on every PR"];
const COVERAGE_INPUTS = [".github/workflows", ".github/actions", "e2e", "mobile/.maestro", "mobile/scripts/maestro-run.sh", "pollis-tui/tests"];

const stripComments = (t) => t.split("\n").filter((l) => !/^\s*#/.test(l)).map((l) => l.replace(/\s+#\s.*$/, "")).join("\n");

function workflowTrigger(text) {
  const on = /^on:\s*\n([\s\S]*?)(?=^\S)/m.exec(text)?.[1] ?? /^on:\s*(.+)$/m.exec(text)?.[1] ?? "";
  if (/\bpull_request\b/.test(on)) {
    return { rank: 4, label: "every PR" };
  }
  if (/\bpush:[\s\S]*?\bbranches:/.test(on) || /\bschedule:/.test(on)) {
    return { rank: 3, label: /\bschedule:/.test(on) ? "nightly/main" : "main" };
  }
  if (/\bpush:[\s\S]*?\btags:/.test(on)) {
    return { rank: 3, label: "release tag" };
  }
  return { rank: 3, label: "manual dispatch" };
}

function jobsOf(text) {
  const body = text.slice(text.search(/^jobs:\s*$/m));
  const parts = body.split(/^ {2}([\w-]+):\s*$/m);
  const jobs = [];
  for (let i = 1; i < parts.length; i += 2) {
    jobs.push({ name: parts[i], text: parts[i + 1], runsOn: /runs-on:\s*([^\n]+)/.exec(parts[i + 1])?.[1].trim() ?? "" });
  }
  return jobs;
}

function platformMatrix() {
  const axes = surfaces.axes;
  const skinned = new Set(axes.skin_applies_to ?? []);
  const cells = new Map();
  for (const p of axes.platform) {
    for (const s of skinned.has(p) ? axes.skin : ["-"]) {
      cells.set(`${p}|${s}`, { platform: p, skin: s, best: 0, sources: [] });
    }
  }
  const credit = (platform, skins, level, source) => {
    for (const s of skinned.has(platform) ? skins ?? axes.skin : ["-"]) {
      const c = cells.get(`${platform}|${s}`);
      if (!c) {
        continue;
      }
      c.best = Math.max(c.best, level);
      c.sources.push({ level, source });
    }
  };

  // The desktop renderer's default skin, and which native e2e scenarios switch to the other one.
  const defaultSkin = /return v === "(\w+)" \? "\w+" : "(\w+)"/.exec(exists("frontend/src/utils/colorUtils.ts") ? read("frontend/src/utils/colorUtils.ts") : "")?.[2] ?? axes.skin[0];
  const scenarioSkins = (name) => {
    const f = `e2e/${name}.js`;
    const text = exists(f) ? read(f) : "";
    return [...new Set([defaultSkin, ...axes.skin.filter((s) => s !== defaultSkin && new RegExp(`["']${s}["']`).test(text))])];
  };

  const sources = [];
  const wiredScenarios = new Set();
  let maestroInCi = false;
  const wfDir = ".github/workflows";
  const workflows = exists(wfDir) ? readdirSync(join(ROOT, wfDir)).filter((f) => /\.ya?ml$/.test(f)).sort() : [];
  let playwright = null;
  for (const wf of workflows) {
    const path = `${wfDir}/${wf}`;
    const text = stripComments(read(path));
    const trig = workflowTrigger(text);
    for (const job of jobsOf(text)) {
      const os = /windows/.test(job.runsOn) ? "windows" : /macos/.test(job.runsOn) ? "macos" : /ubuntu|linux/.test(job.runsOn) ? "linux" : "";
      const where = `${path} → ${job.name}`;
      // Native desktop e2e: the real Tauri binary under tauri-driver, in Xvfb (X11) unless the job brings up Wayland.
      const scenarios = [...job.text.matchAll(/pnpm e2e ([\w-]+)/g)].map((m) => m[1]);
      if (scenarios.length && /desktop-e2e|xvfb-run/.test(job.text) && os === "linux") {
        const platform = /WAYLAND_DISPLAY|weston|\bcage\b|sway/.test(job.text) ? "linux-wayland" : "linux-x11";
        for (const sc of scenarios) {
          wiredScenarios.add(sc);
          const skins = scenarioSkins(sc);
          credit(platform, skins, trig.rank, `${sc} (${trig.label})`);
          sources.push({ kind: "desktop e2e (WebDriver)", where, platform, skins, trigger: trig.label, what: `pnpm e2e ${sc}` });
        }
      }
      if (/e2e:ui|playwright test/.test(job.text)) {
        playwright = { where, trigger: trig.label };
      }
      if (/cargo test[^\n]*-p pollis-tui/.test(job.text)) {
        const e2e = exists("pollis-tui/tests") && readdirSync(join(ROOT, "pollis-tui/tests")).some((f) => /e2e/.test(f));
        credit("tui", null, e2e ? trig.rank : 1, `cargo test -p pollis-tui on ${os || job.runsOn} (${trig.label})`);
        sources.push({ kind: e2e ? "TUI headless e2e" : "TUI unit", where, platform: "tui", trigger: trig.label, what: `cargo test -p pollis-tui on ${os || job.runsOn}` });
      }
      // A path filter naming mobile/.maestro is not a run: only an invocation counts.
      if (/maestro-run\.sh|maestro test\b/.test(job.text)) {
        maestroInCi = true;
      }
      const tauriBuild = /tauri-action|tauri build|build:tauri/.test(job.text);
      // A job that builds only the terminal client (cli-release.yml) says nothing about the desktop app.
      const tuiOnly = /-p pollis-tui\b/.test(job.text) && !tauriBuild && !/-p pollis-core\b|-p pollis(?![\w-])/.test(job.text);
      if (tuiOnly && /cargo build/.test(job.text)) {
        credit("tui", null, 1, `${wf} build on ${os} (${trig.label})`);
      } else if (/cargo (test|build)/.test(job.text) || tauriBuild) {
        if (os === "windows") {
          credit("windows", null, 1, `${wf} (${trig.label})`);
          sources.push({ kind: "build/unit", where, platform: "windows", trigger: trig.label });
        }
        if (os === "macos" && !/apple-ios/.test(job.text)) {
          credit("macos", null, 1, `${wf} (${trig.label})`);
          sources.push({ kind: "build/unit", where, platform: "macos", trigger: trig.label });
        }
        if (/apple-ios/.test(job.text)) {
          credit("ios-iphone", null, 1, `${wf} (${trig.label})`);
          credit("ios-ipad", null, 1, `${wf} (${trig.label})`);
          sources.push({ kind: "build/unit", where, platform: "ios", trigger: trig.label });
        }
        if (os === "linux" && tauriBuild) {
          credit("linux-x11", null, 1, `${wf} (${trig.label})`);
          credit("linux-wayland", null, 1, `${wf} (${trig.label})`);
          sources.push({ kind: "build/unit", where, platform: "linux", trigger: trig.label });
        }
      }
      if (/assembleRelease|cargo ndk|linux-android/.test(job.text)) {
        credit("android", null, 1, `${wf} (${trig.label})`);
        sources.push({ kind: "build/unit", where, platform: "android", trigger: trig.label });
      }
    }
  }

  // Native scenarios no workflow runs: by hand only (e2e/Dockerfile, Xvfb).
  const allScenarios = exists("e2e/run.js") ? readdirSync(join(ROOT, "e2e")).filter((f) => f.endsWith(".js") && !/\.spec\.js$|^run\.js$|config\.js$/.test(f)).map((f) => f.replace(/\.js$/, "")) : [];
  const localScenarios = allScenarios.filter((s) => !wiredScenarios.has(s) && /tauri-driver|WebDriver|require\("\.\/lib|driver/.test(read(`e2e/${s}.js`)));
  for (const sc of localScenarios) {
    credit("linux-x11", scenarioSkins(sc), 2, `${sc} (by hand)`);
  }
  if (localScenarios.length) {
    sources.push({ kind: "desktop e2e (WebDriver)", where: "e2e/*.js (no workflow)", platform: "linux-x11", trigger: "by hand", what: localScenarios.join(", ") });
  }

  // Maestro: mobile e2e. The runner's usage line names the platforms it drives.
  const flowsDir = "mobile/.maestro/flows";
  if (exists(flowsDir)) {
    const flows = readdirSync(join(ROOT, flowsDir)).filter((f) => f.endsWith(".yaml"));
    const runner = exists("mobile/scripts/maestro-run.sh") ? read("mobile/scripts/maestro-run.sh") : "";
    const named = (/<flow\|all> <([\w|]+)>/.exec(runner)?.[1] ?? "").split("|").filter(Boolean);
    const map = { ios: "ios-iphone", ipad: "ios-ipad", android: "android" };
    const level = maestroInCi ? 3 : 2;
    const ipadOnly = flows.filter((f) => /^\s*-\s*ipad\s*$/m.test(read(`${flowsDir}/${f}`)));
    for (const n of named) {
      if (map[n]) {
        credit(map[n], null, level, `Maestro ${flows.length} flow(s) (${maestroInCi ? "CI" : "by hand"})`);
      }
    }
    sources.push({ kind: "mobile e2e (Maestro)", where: `${flowsDir} (${flows.length} flows, ${ipadOnly.length} iPad-only) via mobile/scripts/maestro-run.sh`, platform: named.map((n) => map[n] ?? n).join(", "), trigger: maestroInCi ? "CI" : "by hand (no workflow runs Maestro)" });
  }

  // The Playwright tier drives the renderer in Chromium with Tauri mocked: skin coverage, not platform coverage.
  let renderer = null;
  const specs = exists("e2e") ? readdirSync(join(ROOT, "e2e")).filter((f) => /\.spec\.(js|ts)$/.test(f)) : [];
  if (specs.length) {
    const per = Object.fromEntries(axes.skin.map((s) => [s, specs.filter((f) => new RegExp(`["']${s}["']`).test(read(`e2e/${f}`))).length]));
    renderer = { specs: specs.length, per_skin: per, where: playwright?.where ?? "e2e/playwright.config.js (no workflow)", trigger: playwright?.trigger ?? "by hand" };
  }

  const list = [...cells.values()];
  const gaps = list.filter((c) => c.best <= 1);
  const weak = list.filter((c) => c.best === 2 || c.best === 3);

  // Features whose behaviour varies by platform/skin, on the cells with no e2e.
  const platformsOf = (feat) => {
    const all = feat.platforms ?? axes.platform;
    const excluded = Object.keys(feat.excluded ?? {});
    const groups = { mobile: ["ios-iphone", "ios-ipad", "android"], desktop: [...skinned] };
    const out = all.filter((p) => !excluded.some((e) => e === p || (groups[e] ?? []).includes(p)));
    return out;
  };
  const atRisk = [];
  for (const [id, feat] of Object.entries(surfaces.features ?? {})) {
    const ps = new Set(platformsOf(feat));
    const hit = gaps.filter((c) => ps.has(c.platform) && (c.skin === "-" || (feat.varies_on ?? []).includes("skin") || c.skin === defaultSkin));
    if (hit.length) {
      atRisk.push({ feature: id, cells: hit.map((c) => (c.skin === "-" ? c.platform : `${c.platform}×${c.skin}`)) });
    }
  }
  return { levels: LEVELS, default_skin: defaultSkin, cells: list.map(({ platform, skin, best, sources: s }) => ({ platform, skin, level: LEVELS[best], sources: [...new Set([...s].sort((a, b) => b.level - a.level).map((x) => x.source))] })), gaps: gaps.map((c) => (c.skin === "-" ? c.platform : `${c.platform}×${c.skin}`)), weak: weak.map((c) => `${c.skin === "-" ? c.platform : `${c.platform}×${c.skin}`} (${LEVELS[c.best]})`), renderer, sources, at_risk: atRisk };
}

function printMatrix(m) {
  const pad = (s, n) => String(s).padEnd(n);
  console.log(`    ${pad("platform", 15)}${pad("skin", 10)}coverage`);
  for (const c of m.cells) {
    const src = c.sources.slice(0, 3).join("; ") + (c.sources.length > 3 ? `; +${c.sources.length - 3}` : "");
    console.log(`    ${pad(c.platform, 15)}${pad(c.skin, 10)}${pad(c.level, 24)}${src}`);
  }
  if (m.renderer) {
    console.log(`    renderer: ${m.renderer.specs} Playwright spec(s) in Chromium with Tauri mocked (${m.renderer.trigger}); skin-specific: ${Object.entries(m.renderer.per_skin).map(([s, n]) => `${s} ${n}`).join(", ")}. Not counted as platform coverage.`);
  }
  console.log(`    gaps (no e2e at all): ${m.gaps.join(", ") || "none"}`);
  console.log(`    weak (e2e not on every PR): ${m.weak.join(", ") || "none"}`);
  for (const r of m.at_risk) {
    console.log(`    at risk: ${r.feature} on ${r.cells.join(", ")}`);
  }
}

function dueMatrix(base, diff) {
  const lines = [];
  const feats = Object.entries(surfaces?.features ?? {}).filter(([, f]) => diff.files.some((x) => matchesAny(f.paths ?? [], x))).map(([id]) => id);
  if (feats.length) {
    lines.push(`${feats.length} platform-varying feature(s) changed: ${listShort(feats)}; check each on the platforms the matrix leaves uncovered`);
  }
  const inputs = diff.files.filter((f) => matchesAny(COVERAGE_INPUTS, f));
  if (inputs.length) {
    lines.push(`${inputs.length} CI/e2e file(s) changed: re-read the matrix for new or lost coverage`);
  }
  return { lines, count: feats.length };
}

// ---------------------------------------------------------------------------
// Running
// ---------------------------------------------------------------------------
function built(d) {
  return d.files.every(exists);
}

function runCheck(spec) {
  const c = Array.isArray(spec) ? { argv: spec } : spec;
  const label = c.argv.join(" ");
  if (c.slow && opts["--quick"]) {
    return { label, status: "SKIPPED", detail: "slow; skipped by --quick" };
  }
  const t0 = Date.now();
  const r = spawnSync(c.argv[0], c.argv.slice(1), { cwd: ROOT, encoding: "utf8", maxBuffer: 1 << 28 });
  const secs = ((Date.now() - t0) / 1000).toFixed(1);
  if (r.error) {
    return { label, status: "FAIL", detail: `could not run: ${r.error.message}`, secs };
  }
  const out = `${r.stdout ?? ""}${r.stderr ?? ""}`.trim();
  if (c.parity) {
    // feature-parity reports and never fails: unexplained gaps are findings, not a broken checker.
    let gaps = 0;
    try {
      const j = JSON.parse(r.stdout);
      gaps = (j.desktop_only?.length ?? 0) + (j.mobile_only?.length ?? 0) + (j.missing_from_cli?.length ?? 0);
    } catch {
      return { label, status: "FAIL", detail: "could not parse --json output", secs };
    }
    return { label: "node scripts/feature-parity.mjs", status: gaps ? "FINDINGS" : "PASS", detail: gaps ? `${gaps} unexplained gap(s): run it without --json` : "no unexplained gaps", secs };
  }
  const lines = out.split("\n").filter((l) => l.trim());
  if (r.status === 0) {
    const stdout = (r.stdout ?? "").split("\n").filter((l) => l.trim());
    const warned = /⚠|warn/i.test(out) ? " (with warnings: run it for detail)" : "";
    return { label, status: "PASS", detail: (stdout[stdout.length - 1] ?? lines[lines.length - 1] ?? "").trim() + warned, secs };
  }
  const errs = lines.filter((l) => /error|fail|✗|FAIL|not found|stale|mismatch|panicked/i.test(l));
  return { label, status: "FAIL", detail: (errs.length ? errs : lines).slice(0, 8).map((l) => l.trim()).join("\n"), secs };
}

function dimensionReport(d) {
  const rep = { id: d.id, ticket: d.ticket, what: d.what, built: built(d), checks: [], due: [], dueCount: 0 };
  if (!rep.built) {
    rep.status = "NOT BUILT";
    rep.missing = d.files.filter((f) => !exists(f));
    return rep;
  }
  if (!d.checks && !d.internal) {
    rep.status = "UNWIRED";
    return rep;
  }
  if (RUN_CHECKS) {
    if (d.internal === "registry") {
      const reg = checkRegistry();
      rep.registry = reg;
      rep.checks.push({ label: "registration (both directions)", status: reg.problems.length ? "FAIL" : "PASS",
        detail: reg.problems.length ? reg.problems.join("\n") : `${reg.counts.content} content item(s), ${reg.counts.stores} store(s), ${reg.counts.scanned} public file(s) all registered` });
    } else if (d.internal === "matrix") {
      rep.matrix = typeof platformMatrix === "function" ? platformMatrix() : null;
    }
    for (const c of d.checks ?? []) {
      rep.checks.push(runCheck(c));
    }
  }
  // A mechanical dimension is its checker: nothing is left for judgment once it passes, so it keeps no ledger entry.
  const base = d.mechanical ? null : baseFor(d.id);
  rep.base = base;
  if (d.mechanical) {
    rep.mechanical = true;
  } else if (!base) {
    rep.neverSwept = true;
  } else if (base.error) {
    rep.due.push(base.error);
    rep.dueCount = 1;
  } else if (d.due) {
    const diff = changedSince(base.sha);
    const r = d.due(base, diff);
    rep.due.push(...r.lines);
    rep.dueCount = r.count;
    rep.changedFiles = diff.files.length;
  }
  if (d.manual) {
    rep.manual = d.manual;
  }
  const statuses = rep.checks.map((c) => c.status);
  rep.status = statuses.includes("FAIL") ? "FAIL" : statuses.includes("FINDINGS") ? "FINDINGS" : statuses.length ? (statuses.every((s) => s === "SKIPPED") ? "SKIPPED" : "PASS") : RUN_CHECKS ? "REPORT" : "-";
  return rep;
}

// --list
if (opts["--list"]) {
  for (const d of DIMENSIONS) {
    const checks = (d.checks ?? []).map((c) => (Array.isArray(c) ? c : c.argv).join(" "));
    console.log(`${d.id.padEnd(16)} ${d.ticket.padEnd(6)} ${built(d) ? "built    " : "NOT BUILT"} ${d.what}`);
    console.log(`${" ".repeat(24)}needs: ${d.files.join(", ")}`);
    if (checks.length) {
      console.log(`${" ".repeat(24)}runs:  ${checks.join("; ")}`);
    }
    const l = ledger[d.id];
    console.log(`${" ".repeat(24)}last swept: ${l ? `${l.sha.slice(0, 8)} ${l.date} by ${l.by}` : "never"}   procedure: /pre-release-sweeps § ${d.id}`);
  }
  process.exit(0);
}

// --list-files: every file the registry check reads (the self-test copies exactly these)
if (opts["--list-files"]) {
  const pats = [
    ...(surfaces?.registration?.globs ?? []),
    ...Object.values(surfaces?.content ?? {}).flatMap(contentFiles),
    ...Object.keys(surfaces?.internal ?? {}),
    ...Object.values(surfaces?.features ?? {}).flatMap((f) => f.paths ?? []),
  ];
  const fixed = [SURFACES_PATH, LEDGER_PATH, "scripts/claims.json", "scripts/facts.json"];
  const out = new Set(fixed.filter(exists));
  for (const f of tracked()) {
    if (matchesAny(pats, f) && exists(f)) {
      out.add(f);
    }
  }
  console.log([...out].sort().join("\n"));
  process.exit(0);
}

// --bump
if (opts["--bump"]) {
  const ids = opts["--bump"].split(",").map((s) => s.trim()).filter(Boolean);
  for (const id of ids) {
    if (!DIMENSIONS.some((d) => d.id === id)) {
      usage(`--bump: unknown dimension "${id}" (node scripts/sweep.mjs --list)`);
    }
  }
  // PRs are squash-merged, so a sweep branch's own commits never reach main. Record the main commit the
  // sweep verified against: --at, else where HEAD left origin/main, else HEAD.
  let at = headSha;
  let how = "HEAD";
  try {
    at = git("rev-parse", "--verify", `${opts["--at"] ?? git("merge-base", "HEAD", "origin/main").trim()}^{commit}`).trim();
    how = opts["--at"] ? `--at ${opts["--at"]}` : "merge-base with origin/main";
  } catch {
    if (opts["--at"]) {
      usage(`cannot resolve --at ${opts["--at"]}`);
    }
  }
  const date = new Date().toISOString().slice(0, 10);
  for (const id of ids) {
    ledger[id] = { sha: at, date, by: opts["--by"] ?? "pre-release-sweeps" };
  }
  writeFileSync(join(ROOT, LEDGER_PATH), JSON.stringify(ledger, null, 2) + "\n");
  console.log(`${LEDGER_PATH}: ${ids.join(", ")} → ${at.slice(0, 8)} (${how}, ${date})`);
  process.exit(0);
}

const selected = positional.length ? positional : DIMENSIONS.map((d) => d.id);
for (const id of selected) {
  if (!DIMENSIONS.some((d) => d.id === id)) {
    usage(`unknown dimension "${id}" (node scripts/sweep.mjs --list)`);
  }
}
const reports = DIMENSIONS.filter((d) => selected.includes(d.id)).map(dimensionReport);

function summaryLine() {
  const due = reports.filter((r) => r.built && r.dueCount > 0).map((r) => `${r.id} (${r.dueCount})`);
  const never = reports.filter((r) => r.built && r.neverSwept && r.id !== "platform-matrix").map((r) => r.id);
  const notBuilt = reports.filter((r) => !r.built).map((r) => `${r.id} (${r.ticket})`);
  const parts = [due.length ? due.join(", ") : "none"];
  if (never.length) {
    parts.push(`never swept: ${never.join(", ")}`);
  }
  if (notBuilt.length) {
    parts.push(`not yet built: ${notBuilt.join(", ")}`);
  }
  return `sweeps due: ${parts.join("; ")}${due.length || never.length ? " → /pre-release-sweeps" : ""}`;
}

if (SUMMARY) {
  console.log(summaryLine());
  process.exit(0);
}
if (opts["--json"]) {
  console.log(JSON.stringify({ head: headSha, since: opts["--since"] ?? null, dimensions: reports, summary: summaryLine() }, null, 2));
  process.exit(reports.some((r) => r.status === "FAIL") ? 1 : 0);
}

console.log(`Drift sweeps at ${headSha.slice(0, 8)}${opts["--since"] ? ` (due since ${opts["--since"]})` : " (due since each dimension's last sweep, scripts/sweep-ledger.json)"}\n`);
for (const r of reports) {
  const head = `${r.id} (${r.ticket})`.padEnd(26);
  if (!r.built) {
    console.log(`${head} NOT BUILT  ${r.what}: not yet built (${r.ticket}); missing ${r.missing.join(", ")}`);
    continue;
  }
  if (r.status === "UNWIRED") {
    console.log(`${head} UNWIRED    ${r.ticket} has landed but sweep.mjs does not run it yet: add its checker to DIMENSIONS in scripts/sweep.mjs`);
    continue;
  }
  console.log(`${head} ${r.status.padEnd(10)} ${r.what}`);
  for (const c of r.checks) {
    const detail = c.detail.split("\n");
    console.log(`    ${c.status.padEnd(8)} ${c.label}${c.secs ? ` (${c.secs}s)` : ""}${detail[0] ? `: ${detail[0]}` : ""}`);
    for (const l of detail.slice(1)) {
      console.log(`             ${l}`);
    }
  }
  if (r.manual) {
    console.log(`    manual   ${r.manual}`);
  }
  if (r.matrix) {
    printMatrix(r.matrix);
  }
  if (r.mechanical) {
    console.log(`    due      ${r.status === "FAIL" ? "yes: fix what the checker names" : RUN_CHECKS ? "nothing (mechanical: the checker is the whole sweep)" : "run the checker (mechanical: no ledger)"}`);
  } else if (r.neverSwept) {
    console.log(`    due      never swept (no ${LEDGER_PATH} entry): run the full procedure, then --bump ${r.id}`);
  } else if (r.due.length) {
    console.log(`    due      since ${r.base.short} (${r.base.from}):`);
    for (const l of r.due) {
      console.log(`             - ${l}`);
    }
    console.log(`             → /pre-release-sweeps § ${r.id}`);
  } else if (r.base) {
    console.log(`    due      nothing since ${r.base.short} (${r.base.from})`);
  }
}
console.log(`\n${summaryLine()}`);
process.exit(reports.some((r) => r.status === "FAIL") ? 1 : 0);
