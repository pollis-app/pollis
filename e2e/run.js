#!/usr/bin/env node
/*
 * Single entry point for every e2e scenario, so package.json carries ONE script
 * ("e2e") instead of one line per test case — the list of scenarios lives on
 * disk (the *.js files here), not duplicated into package.json.
 *
 * Each scenario is still its own self-contained program that stands up its own
 * process tree (Vite + per-client tauri-driver/WebKitWebDriver + backend); this
 * only ROUTES a name to its file. It is not a test runner — there is deliberately
 * no shared session/lifecycle across scenarios (see e2e/README.md).
 *
 *   pnpm e2e smoke
 *   pnpm e2e two-client-delete
 *   node e2e/run.js two-client-dm-reply     # equivalent, without pnpm
 */
const fs = require("fs");
const path = require("path");

// Scenarios are the top-level *.js files here, minus this dispatcher. (lib/ and
// node_modules are directories, so they never match the .js name filter.)
function scenarios() {
  return fs
    .readdirSync(__dirname)
    .filter((f) => f.endsWith(".js") && f !== "run.js")
    .map((f) => f.replace(/\.js$/, ""))
    .sort();
}

function fail(msg) {
  console.error(`${msg}\n\nscenarios:\n  ${scenarios().join("\n  ")}`);
  process.exit(2);
}

const name = process.argv[2];
if (!name) {
  fail("usage: pnpm e2e <scenario>");
}

const file = path.join(__dirname, `${name}.js`);
if (path.dirname(file) !== __dirname || !fs.existsSync(file)) {
  fail(`unknown scenario "${name}".`);
}

// Run it by requiring it — each scenario invokes its own main() at module load.
require(file);
