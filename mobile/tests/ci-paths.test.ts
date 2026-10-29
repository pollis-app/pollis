/**
 * mobile-core-check.yml runs each job only on a PR that can change it (the
 * `changes` job's `native` / `app` / `tests` filters). A path missing from a
 * filter is a silent false negative: the PR goes green without the job that
 * would have caught it. These tests hold the filters to the two things that
 * define them — the Rust closure the native build compiles, and the files
 * outside mobile/ that these tests read.
 */
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync, readdirSync } from "node:fs";
import { createRequire } from "node:module";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const require = createRequire(import.meta.url);
const here = dirname(fileURLToPath(import.meta.url));
const mobile = join(here, "..");
const repo = join(mobile, "..");
const fingerprint = require("../fingerprint.config.js");
const readWorkflow = (name: string) => readFileSync(join(repo, ".github", "workflows", name), "utf8");
const workflow = readWorkflow("mobile-core-check.yml");

// The quoted patterns of one `filters:` entry (`            native:` …).
function filter(name: string, src = workflow): string[] {
  const block = new RegExp(`\\n {12}${name}:\\n((?: {14}.*\\n| *#.*\\n)+)`).exec(src)?.[1];
  assert.ok(block, `no \`${name}\` filter`);
  return [...block.matchAll(/^ {14}- '([^']+)'$/gm)].map((m) => m[1]);
}

// A trigger's `paths:` list (comment lines between entries allowed).
function triggerPaths(event: "push" | "pull_request", src = workflow): string[] {
  const head = event === "push" ? "push:\\n {4}branches: \\[main\\]\\n(?: *#.*\\n)*" : "pull_request:\\n(?: *#.*\\n)*";
  const block = new RegExp(`\\n {2}${head} {4}paths:\\n((?: {6}- '.*'\\n| *#.*\\n)+)`).exec(src)?.[1];
  assert.ok(block, `no ${event} paths list`);
  return [...block.matchAll(/^ {6}- '([^']+)'$/gm)].map((m) => m[1]);
}
const pushPaths = () => triggerPaths("push");

// picomatch's subset the filters use: `**`, `*`, literal text.
function matches(pattern: string, file: string): boolean {
  const re = pattern
    .split("**")
    .map((part) => part.replace(/[.+^${}()|[\]\\]/g, "\\$&").replace(/\*/g, "[^/]*"))
    .join(".*");
  return new RegExp(`^${re}$`).test(file);
}

const native = filter("native");
const app = filter("app");
const tests = filter("tests");

test("the native filter covers every crate the mobile build compiles", () => {
  for (const crate of fingerprint.rustCrateClosure() as string[]) {
    assert.ok(native.includes(`${crate}/**`), `native filter is missing '${crate}/**'`);
  }
  for (const file of ["Cargo.toml", "Cargo.lock", "rust-toolchain.toml", ".cargo/config.toml"]) {
    assert.ok(native.some((p) => matches(p, file)), `native filter is missing ${file}`);
  }
  assert.ok(native.some((p) => matches(p, "mobile/modules/pollis-native/ubrn.config.yaml")));
  assert.ok(native.some((p) => matches(p, ".github/workflows/mobile-core-check.yml")));
});

test("the app filter covers the JS app and the catalogues Metro bundles", () => {
  assert.ok(app.some((p) => matches(p, "mobile/app/_layout.tsx")));
  assert.ok(app.some((p) => matches(p, "frontend/src/i18n/locales/en/common.json")));
});

test("every file outside mobile/ that these tests read is on a filter", () => {
  const all = [...native, ...app, ...tests];
  const found = new Set<string>();
  for (const name of readdirSync(here)) {
    if (!name.endsWith(".ts")) {
      continue;
    }
    const src = readFileSync(join(here, name), "utf8");
    // `../../x/y` from tests/ is the repo root. A single `../` is mobile/ from
    // tests/ (mobile/scripts/…), but the repo root from a ROOT = mobile join,
    // which only ever reaches frontend/.
    for (const m of src.matchAll(/["'`]\.\.\/\.\.\/((?:frontend|pollis-[a-z-]+|verifiable-log[a-z-]*|scripts|website)\/[^"'`$]+)/g)) {
      found.add(m[1]);
    }
    for (const m of src.matchAll(/["'`]\.\.\/(frontend\/[^"'`$]+)/g)) {
      found.add(m[1]);
    }
    for (const m of src.matchAll(/join\(repo, ((?:"[^"]+", )*"[^"]+")\)/g)) {
      found.add([...m[1].matchAll(/"([^"]+)"/g)].map((p) => p[1]).join("/"));
    }
  }
  assert.ok(found.size > 0);
  for (const file of found) {
    assert.ok(all.some((p) => matches(p, file) || matches(p, `${file}x`)), `${file} is read by mobile/tests but no filter in mobile-core-check.yml covers it`);
  }
});

// windows-link.yml links pollis-core for Windows and kani.yml proves harnesses
// inside it: both compile the same closure, so both lists are held to it too.
test("windows-link and kani run on every crate of pollis-core's closure", () => {
  const closure = (fingerprint.rustCrateClosure() as string[]).map((c) => `${c}/**`);
  const windows = readWorkflow("windows-link.yml");
  const kani = readWorkflow("kani.yml");
  const lists: Array<[string, string[]]> = [
    ["windows-link.yml `rust` filter", filter("rust", windows)],
    ["windows-link.yml push paths", triggerPaths("push", windows)],
    ["kani.yml pull_request paths", triggerPaths("pull_request", kani)],
    ["kani.yml push paths", triggerPaths("push", kani)],
  ];
  for (const [where, list] of lists) {
    for (const p of [...closure, "Cargo.toml", "Cargo.lock", "patches/**", ".cargo/**", "rust-toolchain.toml"]) {
      assert.ok(list.includes(p), `${where} is missing '${p}'`);
    }
  }
});

test("main's push paths are the union of the filters", () => {
  const push = pushPaths();
  for (const p of [...native, ...app, ...tests]) {
    assert.ok(push.some((q) => q === p || matches(q, p)), `push paths are missing '${p}'`);
  }
});
