#!/usr/bin/env node
/**
 * Dead commands: a command registered for a client that nothing in that client calls.
 *
 * rustc cannot see this. A `#[tauri::command]` listed in `generate_handler!` (or a
 * mobile `bridge.rs` match arm) always counts as used, whether or not any TypeScript
 * ever invokes it. Knip cannot see it either, since the call is a string. So the
 * check is a string-literal match, the same approach as scripts/feature-parity.mjs.
 *
 *   node scripts/dead-commands.mjs            # report, exit 0
 *   node scripts/dead-commands.mjs --strict   # exit 1 when anything is unexplained
 *   node scripts/dead-commands.mjs --json     # machine-readable
 *
 * Findings, most to least actionable:
 *   dead              registered somewhere, called by NO client (renderer, mobile, CLI)
 *   desktop_unused    in generate_handler!, the renderer never names it
 *   mobile_unused     a bridge.rs arm, the mobile app never names it
 *
 * A name built at runtime (`invoke(`get_${x}`)`) is invisible to a literal match, so
 * list it in scripts/dead-commands.json with the reason.
 */

import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { join } from "node:path";

const root = execFileSync("git", ["rev-parse", "--show-toplevel"], { encoding: "utf8" }).trim();
const args = process.argv.slice(2);
const strict = args.includes("--strict");
const asJson = args.includes("--json");

function read(path) {
  try {
    return readFileSync(join(root, path), "utf8");
  } catch {
    return "";
  }
}

// Tracked source files under dirs with one of the extensions, tests and mocks excluded:
// a mock that lists every command would make everything look used.
function sources(dirs, exts, keepTests = false) {
  return execFileSync("git", ["ls-files", "--", ...dirs], { cwd: root, encoding: "utf8", maxBuffer: 1 << 28 })
    .split("\n")
    .filter((p) => p && exts.some((e) => p.endsWith(e)))
    .filter((p) => keepTests || !/(^|\/)(tests?|__tests__|__mocks__|e2e|\.maestro)\//.test(p))
    .filter((p) => keepTests || !/\.(test|spec)\.[jt]sx?$/.test(p))
    .map(read);
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

// Desktop: names in the generate_handler! list, last path segment.
const lib = read("src-tauri/src/lib.rs");
const block = lib.slice(lib.indexOf("generate_handler!["));
const handlers = new Set();
for (const line of block.slice(0, block.indexOf("])")).split("\n")) {
  const m = line.trim().match(/^(?:[a-z_0-9]+::)*([a-z_][a-z_0-9]*),?$/);
  if (m) {
    handlers.add(m[1]);
  }
}
const desktopCalls = stringLiterals(sources(["frontend/src"], [".ts", ".tsx"]));

// Mobile: bridge.rs match arms.
const bridge = read("pollis-core/src/bridge.rs");
const arms = new Set([...bridge.matchAll(/^\s*"([a-z_0-9]+)"\s*(?:\|[^=]*)?=>/gm)].map((m) => m[1]));
for (const m of bridge.matchAll(/\|\s*"([a-z_0-9]+)"/g)) {
  arms.add(m[1]);
}
const mobileCalls = stringLiterals(
  sources(["mobile/app", "mobile/components", "mobile/hooks", "mobile/lib", "mobile/stores"], [".ts", ".tsx"]),
);

// CLI: any whole-word mention in pollis-tui. Looser than the parity script on purpose:
// a false "used" only hides a finding, a false "dead" would send someone to delete live code.
const tui = sources(["pollis-tui/src"], [".rs"]).join("\n");
const cliUses = (name) => new RegExp(`\\b${name}\\b`).test(tui);

// Integration tests (the flows suite) call commands directly, so a "dead" command can
// still be exercised by a test. Say so: deleting it means deleting that test too.
const tests = sources(["src-tauri/tests", "pollis-core/tests"], [".rs"], true).join("\n");
const testsUse = (name) => new RegExp(`\\b${name}\\b`).test(tests);

const allow = JSON.parse(read("scripts/dead-commands.json") || "{}");
const exempt = (name) => allow.exempt?.[name] ?? null;

const report = { dead: [], desktop_unused: [], mobile_unused: [] };
for (const name of [...new Set([...handlers, ...arms])].sort()) {
  if (exempt(name)) {
    continue;
  }
  const onDesktop = handlers.has(name);
  const onMobile = arms.has(name);
  const desktopLive = onDesktop && desktopCalls.has(name);
  const mobileLive = onMobile && mobileCalls.has(name);
  if (!desktopLive && !mobileLive && !cliUses(name)) {
    report.dead.push(testsUse(name) ? `${name}  (only integration tests call it)` : name);
    continue;
  }
  if (onDesktop && !desktopLive) {
    report.desktop_unused.push(name);
  }
  if (onMobile && !mobileLive) {
    report.mobile_unused.push(name);
  }
}

const total = Object.values(report).reduce((a, v) => a + v.length, 0);

if (asJson) {
  console.log(JSON.stringify({ registered: { desktop: handlers.size, mobile: arms.size }, ...report }, null, 2));
} else {
  console.log(`Registered commands: desktop ${handlers.size}, mobile ${arms.size}\n`);
  const labels = {
    dead: "Dead: registered, called by no client (remove the handler, the bridge arm and the pollis-core impl)",
    desktop_unused: "In generate_handler!, never named by the renderer (still used by mobile or the CLI)",
    mobile_unused: "Mobile bridge arm, never named by the mobile app (still used by desktop or the CLI)",
  };
  for (const [k, label] of Object.entries(labels)) {
    if (report[k].length > 0) {
      console.log(`${label}:\n  ${report[k].join("\n  ")}\n`);
    }
  }
  if (total === 0) {
    console.log("None. Every registered command is called, or listed in scripts/dead-commands.json.");
  } else {
    console.log("For each: delete it, or add it to scripts/dead-commands.json with the reason it is still needed.");
  }
}

process.exit(strict && total > 0 ? 1 : 0);
