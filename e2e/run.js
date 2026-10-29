#!/usr/bin/env node
// Routes `pnpm e2e <scenario>` to e2e/<scenario>.js; not a test runner.
const fs = require("fs");
const path = require("path");

// Scenarios are the top-level *.js files here, minus this dispatcher.
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
