#!/usr/bin/env node
/**
 * check-clones.mjs — copy-paste ratchet, one code boundary at a time (#1731).
 *
 * Duplication is drift: two copies of a helper diverge, and then a fix lands in
 * only one. `scripts/clones.json` lists the boundaries (frontend, mobile,
 * website, rust; see .codesight/wiki/code-boundaries.md) and what each scans.
 * Every boundary is scanned separately with a pinned jscpd, so a clone across
 * a boundary (a desktop/mobile parity twin) is never reported: sharing across
 * one is a design decision, not a cleanup.
 *
 * `scripts/clones-baseline.json` holds the clones accepted today, by jscpd
 * fingerprint (a hash of the cloned tokens, so moving code does not change
 * it), with a readable location list beside each boundary. The check fails on
 *   - a clone whose fingerprint is not in the baseline (new duplication), or
 *   - a boundary that scans no source files (a path or ignore rotted into a
 *     silent pass), or a configured path that no longer exists.
 * Baseline clones that are gone print as a ratchet hint, not a failure:
 * `--update-baseline` drops them.
 *
 * Usage:
 *   node scripts/check-clones.mjs                      every boundary
 *   node scripts/check-clones.mjs --boundary website   one (comma-separate several)
 *   node scripts/check-clones.mjs --update-baseline    accept the current clones
 *   node scripts/check-clones.mjs --root <dir>         scan another tree (proofs)
 *   node scripts/check-clones.mjs --all                also list baseline clones
 *
 * Not CI and never a merge gate: run by /reuse-check before a PR and by
 * /pre-release-sweeps (.claude/skills/).
 */

import { readFileSync, writeFileSync, existsSync, mkdtempSync, rmSync } from "node:fs";
import { join, resolve, relative, dirname } from "node:path";
import { tmpdir } from "node:os";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const args = process.argv.slice(2);

function flag(name) {
  return args.includes(name);
}

function option(name) {
  const i = args.indexOf(name);
  if (i < 0) {
    return null;
  }
  return args[i + 1] ?? null;
}

const root = resolve(option("--root") ?? join(here, ".."));
const configPath = join(root, "scripts/clones.json");
const baselinePath = join(root, "scripts/clones-baseline.json");
const update = flag("--update-baseline");
const listAll = flag("--all");

const config = JSON.parse(readFileSync(configPath, "utf8"));
const baseline = existsSync(baselinePath)
  ? JSON.parse(readFileSync(baselinePath, "utf8"))
  : { boundaries: {} };

const names = Object.keys(config.boundaries ?? {});
if (names.length === 0) {
  console.error("scripts/clones.json: no boundaries configured. Fix: restore the boundaries map.");
  process.exit(1);
}
const wanted = option("--boundary") ? option("--boundary").split(",") : names;
for (const w of wanted) {
  if (!names.includes(w)) {
    console.error(`unknown boundary "${w}" (have: ${names.join(", ")})`);
    process.exit(2);
  }
}

// Rust keeps unit tests inline (`#[cfg(test)] mod tests { … }`), where a path
// glob cannot reach them. With `skipInlineTests`, a clone whose two copies both
// sit inside such a module is dropped, matching the boundary's ignore of test
// files. A module's extent is found by counting braces outside string, char
// and comment tokens.
const testRanges = new Map();
function inlineTestRanges(file) {
  if (testRanges.has(file)) {
    return testRanges.get(file);
  }
  const lines = readFileSync(file, "utf8").split("\n");
  const ranges = [];
  for (let i = 0; i + 1 < lines.length; i++) {
    if (!/^\s*#\[cfg\(test\)\]/.test(lines[i]) || !/^\s*(pub(\(\w+\))?\s+)?mod\s+\w+\s*\{/.test(lines[i + 1])) {
      continue;
    }
    let depth = 0;
    let end = lines.length;
    scan: for (let j = i + 1; j < lines.length; j++) {
      // Drop comments, string and char literals before counting.
      const code = lines[j]
        .replace(/\/\/.*$/, "")
        .replace(/"(?:[^"\\]|\\.)*"/g, '""')
        .replace(/'(?:[^'\\]|\\.)'/g, "''");
      for (const ch of code) {
        if (ch === "{") {
          depth++;
        } else if (ch === "}") {
          depth--;
          if (depth === 0) {
            end = j + 1;
            break scan;
          }
        }
      }
    }
    ranges.push([i + 1, end]);
    i = end - 1;
  }
  testRanges.set(file, ranges);
  return ranges;
}

function inTests(f) {
  return inlineTestRanges(f.name).some(([s, e]) => f.start >= s && f.end <= e);
}

function loc(f) {
  return `${relative(root, f.name)}:${f.start}-${f.end}`;
}

function runBoundary(name) {
  const b = config.boundaries[name];
  const problems = [];
  for (const p of b.paths) {
    if (!existsSync(join(root, p))) {
      problems.push(`${p}: configured path does not exist. Fix: update scripts/clones.json → boundaries.${name}.paths.`);
    }
  }
  const tmp = mkdtempSync(join(tmpdir(), `clones-${name}-`));
  try {
    const known = baseline.boundaries?.[name]?.fingerprints ?? {};
    const tmpBaseline = join(tmp, "baseline.json");
    writeFileSync(tmpBaseline, JSON.stringify({ version: 1, fingerprints: known }));
    const cli = [
      "dlx",
      `jscpd@${config.jscpd}`,
      ...b.paths.filter((p) => existsSync(join(root, p))),
      "--format", b.format.join(","),
      "--min-tokens", String(b.minTokens ?? config.minTokens),
      "--min-lines", String(b.minLines ?? config.minLines),
      "--baseline", tmpBaseline,
      "--update-baseline",
      "--absolute",
      "--reporters", "json",
      "--output", tmp,
      "--no-tips",
      "--no-colors",
    ];
    if (b.ignore?.length) {
      cli.push("--ignore", b.ignore.join(","));
    }
    const r = spawnSync("pnpm", cli, { cwd: root, encoding: "utf8" });
    const reportPath = join(tmp, "jscpd-report.json");
    if (r.status !== 0 || !existsSync(reportPath)) {
      problems.push(`jscpd failed (exit ${r.status}):\n${r.stderr || r.stdout}`);
      return { name, problems, fresh: [], dups: [], stale: 0, sources: 0, fingerprints: known };
    }
    const report = JSON.parse(readFileSync(reportPath, "utf8"));
    const after = JSON.parse(readFileSync(tmpBaseline, "utf8")).fingerprints ?? {};
    const sources = report.statistics?.total?.sources ?? 0;
    if (sources === 0) {
      problems.push(`scanned no files. Fix: scripts/clones.json → boundaries.${name} (paths, format or ignore) matches nothing.`);
    }
    const dups = (report.duplicates ?? []).filter(
      (d) => !(b.skipInlineTests && inTests(d.firstFile) && inTests(d.secondFile)),
    );
    const fresh = dups.filter((d) => d.isNew);
    const stale = Object.keys(known).filter((k) => !(k in after)).length;
    return { name, problems, fresh, dups, stale, sources, fingerprints: after };
  } finally {
    rmSync(tmp, { recursive: true, force: true });
  }
}

let failed = false;
const results = wanted.map(runBoundary);
for (const r of results) {
  const head = `${r.name}: ${r.dups.length} clone(s) in ${r.sources} file(s)`;
  if (r.problems.length) {
    failed = true;
    console.log(`✗ ${head}`);
    for (const p of r.problems) {
      console.log(`  ${p}`);
    }
  }
  if (r.fresh.length && !update) {
    failed = true;
    console.log(`✗ ${head}, ${r.fresh.length} NEW`);
    for (const d of r.fresh) {
      console.log(`  ${loc(d.firstFile)}  ≡  ${loc(d.secondFile)}  (${d.lines} lines, ${d.tokens} tokens)`);
    }
    console.log("  Fix: reuse the existing code, or extract one copy inside this boundary (/reuse-check).");
    console.log("  Deliberate? Say why in the PR and run with --update-baseline.");
  } else if (!r.problems.length) {
    console.log(update ? `↻ ${head}, ${r.fresh.length} new accepted, ${r.stale} gone` : `✓ ${head}, none new`);
  }
  if (r.stale && !update) {
    console.log(`  ↓ ${r.stale} baseline clone(s) are gone: run with --update-baseline to ratchet down.`);
  }
  if (listAll) {
    for (const d of r.dups) {
      console.log(`    ${loc(d.firstFile)}  ≡  ${loc(d.secondFile)}  (${d.lines} lines)`);
    }
  }
}

if (update) {
  if (results.some((r) => r.problems.length)) {
    console.error("baseline not written: fix the problems above first.");
    process.exit(1);
  }
  const out = {
    _comment: [
      "Accepted clones for scripts/check-clones.mjs (#1731): today's duplication, recorded so that NEW clones stand out.",
      "fingerprints is what the check compares (jscpd's hash of the cloned tokens); clones is the readable list at the time of the last update.",
      "Rewrite with `node scripts/check-clones.mjs --update-baseline`. Removing a clone shrinks it; adding one needs a reason in the PR.",
    ],
    boundaries: { ...(baseline.boundaries ?? {}) },
  };
  for (const r of results) {
    out.boundaries[r.name] = {
      count: r.dups.length,
      fingerprints: Object.fromEntries(Object.entries(r.fingerprints).sort(([a], [b]) => a.localeCompare(b))),
      clones: r.dups
        .map((d) => `${loc(d.firstFile)} ≡ ${loc(d.secondFile)}`)
        .sort(),
    };
  }
  const ordered = { _comment: out._comment, boundaries: {} };
  for (const n of names) {
    if (out.boundaries[n]) {
      ordered.boundaries[n] = out.boundaries[n];
    }
  }
  writeFileSync(baselinePath, JSON.stringify(ordered, null, 2) + "\n");
  console.log(`wrote ${relative(process.cwd(), baselinePath)}`);
}

process.exit(failed && !update ? 1 : 0);
