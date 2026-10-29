#!/usr/bin/env node
/**
 * Feature parity (#1347): core commands one client calls and another does not, minus scripts/feature-parity.json.
 *   node scripts/feature-parity.mjs                 # every unexplained gap today
 *   node scripts/feature-parity.mjs --since v1.17.2 # only gaps NEW since a ref:
 *                                                   #   "this landed on one client only"
 *   node scripts/feature-parity.mjs --debt          # also list the known parity debt
 *   node scripts/feature-parity.mjs --json          # machine-readable
 *
 * It reports; it never fails the build. Exit status is always 0.
 */

import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { join } from "node:path";

const root = execFileSync("git", ["rev-parse", "--show-toplevel"], { encoding: "utf8" }).trim();
const args = process.argv.slice(2);
const since = args.includes("--since") ? args[args.indexOf("--since") + 1] : null;
const asJson = args.includes("--json");
const showDebt = args.includes("--debt");

const git = (...a) => execFileSync("git", a, { cwd: root, encoding: "utf8", maxBuffer: 1 << 28 });

// Read files at a ref, or from the working tree when ref is null.
function files(ref, dirs, exts) {
  const list = ref
    ? git("ls-tree", "-r", "--name-only", ref, "--", ...dirs)
    : git("ls-files", "--", ...dirs);
  return list
    .split("\n")
    .filter((p) => p && exts.some((e) => p.endsWith(e)))
    .filter((p) => !/(^|\/)(tests?|__tests__|__mocks__|e2e|\.maestro)\//.test(p))
    .filter((p) => !/\.(test|spec)\.[jt]sx?$/.test(p))
    .map((p) => {
      try {
        return ref ? git("show", `${ref}:${p}`) : readFileSync(join(root, p), "utf8");
      } catch {
        return "";
      }
    });
}

function one(ref, path) {
  return files(ref, [path], [""])[0] ?? "";
}

function stringLiterals(texts) {
  const out = new Set();
  for (const t of texts) {
    for (const m of t.matchAll(/["'`]([a-z][a-z0-9_]+)["'`]/g)) {
      out.add(m[1]);
    }
  }
  return out;
}

function snapshot(ref) {
  // Desktop: registered handler names, last path segment.
  const lib = one(ref, "src-tauri/src/lib.rs");
  const block = lib.slice(lib.indexOf("generate_handler!["));
  const handlers = new Set();
  for (const line of block.slice(0, block.indexOf("])")).split("\n")) {
    const m = line.trim().match(/^(?:[a-z_0-9]+::)*([a-z_][a-z_0-9]*),?$/);
    if (m) {
      handlers.add(m[1]);
    }
  }
  const desktopLits = stringLiterals(files(ref, ["frontend/src"], [".ts", ".tsx"]));
  const desktop = new Set([...handlers].filter((n) => desktopLits.has(n)));

  // Mobile: bridge match arms.
  const bridge = one(ref, "pollis-core/src/bridge.rs");
  const arms = new Set([...bridge.matchAll(/^\s*"([a-z_0-9]+)"\s*(?:\|[^=]*)?=>/gm)].map((m) => m[1]));
  for (const m of bridge.matchAll(/\|\s*"([a-z_0-9]+)"/g)) {
    arms.add(m[1]);
  }
  const mobileLits = stringLiterals(
    files(ref, ["mobile/app", "mobile/components", "mobile/hooks", "mobile/lib", "mobile/stores"], [".ts", ".tsx"]),
  );
  const mobile = new Set([...arms].filter((n) => mobileLits.has(n)));

  // CLI: direct calls into pollis_core::commands.
  const cli = new Set();
  const tui = files(ref, ["pollis-tui/src"], [".rs"]);
  const modules = new Set();
  for (const t of tui) {
    // Forms seen in pollis-tui:
    //   use pollis_core::commands::{auth, pin};                 → modules
    //   use pollis_core::commands::dm::{self, DmChannel};       → module dm
    //   use pollis_core::commands::mls::{poll_mls_welcomes};    → fn
    //   pollis_core::commands::user::search_user_by_username(…) → fn (qualified call)
    //   use pollis_core::commands::auth;                        → module
    const re = /pollis_core::commands((?:::[a-z_0-9]+)*)(::\{([^}]*)\}|\s*\()?/g;
    for (const m of t.matchAll(re)) {
      const path = m[1].split("::").filter(Boolean);
      const last = path[path.length - 1];
      if (m[3] !== undefined) {
        for (const item of m[3].split(",").map((s) => s.trim().split(/\s+as\s+/)[0])) {
          if (item === "self" && last) {
            modules.add(last);
          } else if (/^[a-z_0-9]+$/.test(item)) {
            (path.length === 0 ? modules : cli).add(item);
          }
        }
      } else if (m[2] && last) {
        cli.add(last);
      } else if (path.length === 1) {
        modules.add(last);
      } else if (last) {
        cli.add(last);
      }
    }
  }
  for (const t of tui) {
    for (const m of t.matchAll(/\b([a-z_0-9]+)::([a-z_0-9]+)\s*\(/g)) {
      if (modules.has(m[1])) {
        cli.add(m[2]);
      }
    }
  }
  return { desktop, mobile, cli };
}

const allow = JSON.parse(readFileSync(join(root, "scripts/feature-parity.json"), "utf8"));

function exempt(scope, name) {
  if (allow[scope]?.[name]) {
    return allow[scope][name];
  }
  for (const p of allow.patterns ?? []) {
    if (p.scopes.includes(scope) && new RegExp(p.match).test(name)) {
      return p.reason;
    }
  }
  return null;
}

function isDebt(scope, name) {
  return (allow.known_debt?.[scope] ?? []).includes(name);
}

// Every unexplained difference, before known debt is set aside.
function gaps(s) {
  const out = { desktop_only: [], mobile_only: [], missing_from_cli: [] };
  for (const n of s.desktop) {
    if (!s.mobile.has(n) && !exempt("desktop_only", n)) {
      out.desktop_only.push(n);
    }
  }
  for (const n of s.mobile) {
    if (!s.desktop.has(n) && !exempt("mobile_only", n)) {
      out.mobile_only.push(n);
    }
  }
  // The CLI is a deliberately smaller client: only commands BOTH apps use are
  // expected there, and its exemptions are listed like the others.
  for (const n of s.desktop) {
    if (s.mobile.has(n) && !s.cli.has(n) && !exempt("missing_from_cli", n)) {
      out.missing_from_cli.push(n);
    }
  }
  for (const k of Object.keys(out)) {
    out[k].sort();
  }
  return out;
}

const now = snapshot(null);
const all = gaps(now);
const debt = {};
let report = {};
for (const k of Object.keys(all)) {
  debt[k] = all[k].filter((n) => isDebt(k, n));
  report[k] = all[k].filter((n) => !isDebt(k, n));
}
let heading = "New parity gaps (not exempt, not known debt)";

if (since) {
  // Only what became a gap after `since`: a command newly used on one client
  // that the others do not use. Gaps that already existed are parity debt,
  // reported by the plain run.
  const before = gaps(snapshot(since));
  for (const k of Object.keys(all)) {
    const old = new Set(before[k]);
    report[k] = all[k].filter((n) => !old.has(n));
  }
  heading = `Parity gaps introduced since ${since}`;
}

if (asJson) {
  console.log(JSON.stringify({ since, counts: { desktop: now.desktop.size, mobile: now.mobile.size, cli: now.cli.size }, ...report, known_debt: debt }, null, 2));
  process.exit(0);
}

console.log(`${heading}`);
console.log(`(core commands in use: desktop ${now.desktop.size}, mobile ${now.mobile.size}, CLI ${now.cli.size})\n`);
const labels = {
  desktop_only: "Desktop uses, mobile does not (CLAUDE.md: desktop and mobile ship together)",
  mobile_only: "Mobile uses, desktop does not",
  missing_from_cli: "Both apps use, the CLI does not",
};
let any = false;
for (const [k, label] of Object.entries(labels)) {
  if (report[k].length === 0) {
    continue;
  }
  any = true;
  console.log(`${label}:`);
  for (const n of report[k]) {
    console.log(`  ${n}`);
  }
  console.log("");
}
if (!any) {
  console.log("None. Every difference is a documented exemption or already-known debt.");
} else {
  console.log("For each: wire it into the other client, or add it to scripts/feature-parity.json with the reason.");
}
// Debt that has been paid off since the baseline: the list should shrink.
const stale = [];
for (const [k, names] of Object.entries(allow.known_debt ?? {})) {
  for (const n of names) {
    if (!all[k]?.includes(n)) {
      stale.push(`${k}: ${n}`);
    }
  }
}
const debtCount = Object.values(debt).reduce((a, v) => a + v.length, 0);
console.log(`\nKnown parity debt: ${debtCount} (desktop-only ${debt.desktop_only.length}, mobile-only ${debt.mobile_only.length}, missing from CLI ${debt.missing_from_cli.length})${showDebt ? "" : ". --debt lists it."}`);
if (showDebt) {
  for (const [k, label] of Object.entries(labels)) {
    if (debt[k].length > 0) {
      console.log(`\n${label}:\n  ${debt[k].join("\n  ")}`);
    }
  }
}
if (stale.length > 0) {
  console.log(`\nNo longer a gap, so remove from known_debt in scripts/feature-parity.json:\n  ${stale.join("\n  ")}`);
}
