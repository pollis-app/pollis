// Unit tests for scripts/oss-inventory.mjs (#1832): the licence gate's SPDX
// logic, the VEX version ranges, and the npm walker on a pnpm-shaped tree.
// Run: node --test scripts/test-oss-inventory.mjs

import { test } from "node:test";
import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, symlinkSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { jsGraph, normaliseLicense, parseSpdx, spdxAllowed, versionAffected } from "./oss-inventory.mjs";

const ALLOWED = new Set(["MIT", "Apache-2.0", "BSD-3-Clause", "Apache-2.0 WITH LLVM-exception"]);
const allowed = (expr) => spdxAllowed(parseSpdx(normaliseLicense(expr)), ALLOWED);

test("an OR needs one allowed branch, an AND needs all", () => {
  assert.equal(allowed("MIT OR GPL-3.0-only"), true);
  assert.equal(allowed("MIT AND GPL-3.0-only"), false);
  assert.equal(allowed("(MIT OR GPL-2.0) AND BSD-3-Clause"), true);
  assert.equal(allowed("GPL-3.0-only"), false);
});

test("a WITH exception is allowed only as listed, never by its base licence", () => {
  assert.equal(allowed("Apache-2.0 WITH LLVM-exception"), true);
  assert.equal(allowed("MIT WITH LLVM-exception"), false);
});

test("cargo's and npm's slash spelling means OR", () => {
  assert.equal(normaliseLicense("MIT/Apache-2.0"), "MIT OR Apache-2.0");
  assert.equal(normaliseLicense("Apache-2.0 / MIT"), "Apache-2.0 OR MIT");
  assert.equal(allowed("GPL-3.0/MIT"), true);
});

test("strings that are not SPDX expressions do not parse, so the gate fails them", () => {
  for (const bad of ["SEE LICENSE IN LICENSE.md", "MIT OR", "(MIT", "Apache 2.0", ""]) {
    assert.equal(parseSpdx(bad), null, bad);
  }
});

test("VEX ranges match the advisory's vulnerable versions only", () => {
  assert.equal(versionAffected("0.5.5", ["<0.7.6"]), true);
  assert.equal(versionAffected("0.7.6", ["<0.7.6"]), false);
  assert.equal(versionAffected("0.3.27", [">=0.3.0, <0.3.28"]), true);
  assert.equal(versionAffected("0.4.0", [">=0.3.0, <0.3.28"]), false);
  assert.equal(versionAffected("9.9.9", []), true);
});

// A pnpm layout: the project links its direct deps into .pnpm/<pkg>@<v>/node_modules/<pkg>,
// and each package's own deps sit beside it there.
function pnpmTree() {
  const root = mkdtempSync(join(tmpdir(), "oss-inventory-"));
  const store = join(root, "node_modules/.pnpm");
  const pkg = (name, version, extra = {}, files = {}) => {
    const dir = join(store, `${name.replace("/", "+")}@${version}`, "node_modules", name);
    mkdirSync(dir, { recursive: true });
    writeFileSync(join(dir, "package.json"), JSON.stringify({ name, version, ...extra }));
    for (const [f, text] of Object.entries(files)) {
      writeFileSync(join(dir, f), text);
    }
    return dir;
  };
  const a = pkg("a", "1.0.0", { license: "MIT", dependencies: { b: "^2" } }, { LICENSE: "a licence" });
  const b = pkg("b", "2.0.0", { license: { type: "ISC" } });
  pkg("devonly", "1.0.0", { license: "GPL-3.0-only" });
  // b sits next to a in a's .pnpm folder.
  symlinkSync(b, join(store, "a@1.0.0/node_modules/b"));
  mkdirSync(join(root, "app/node_modules"), { recursive: true });
  symlinkSync(a, join(root, "app/node_modules/a"));
  writeFileSync(
    join(root, "app/package.json"),
    JSON.stringify({ name: "app", dependencies: { a: "^1" }, devDependencies: { devonly: "^1" } }),
  );
  return join(root, "app");
}

test("the npm walk follows runtime deps transitively and never dev deps", () => {
  const { components, edges } = jsGraph([{ dir: pnpmTree() }]);
  const byName = Object.fromEntries([...components.values()].map((c) => [c.name, c]));
  assert.deepEqual(Object.keys(byName).sort(), ["a", "b"]);
  assert.equal(byName.a.license, "MIT");
  assert.equal(byName.b.license, "ISC");
  assert.equal(byName.a.purl, "pkg:npm/a@1.0.0");
  assert.deepEqual(byName.a.texts, ["a licence"]);
  assert.ok(edges.has("a@1.0.0>b@2.0.0"));
});
