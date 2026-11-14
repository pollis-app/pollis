// Unit tests for scripts/changelog-section.mjs (#1916): the release's notes come
// out of CHANGELOG.md, and nothing about them can fail a release.
// Run: node --test scripts/test-changelog-section.mjs

import { test } from "node:test";
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { changelogSection } from "./changelog-section.mjs";

const HERE = dirname(fileURLToPath(import.meta.url));
const SCRIPT = join(HERE, "changelog-section.mjs");
const CHANGELOG = join(HERE, "..", "CHANGELOG.md");

const SAMPLE = `# Changelog

Intro text, with a rule under it.

---

## v2.1.0 — 2026-12-01

- **First** change,
  wrapped onto a second line.
- Second change with \`code\`.

A closing paragraph.

## v2.0.1 – v2.0.9

These releases shipped together.

## v2.0.0 — 2026-11-20

- The 2.0.0 change.

## v1.9.0

- An entry without a date.

---

## Earlier releases

Not an entry.
`;

test("an entry's body comes back without its heading", () => {
  assert.equal(
    changelogSection(SAMPLE, "v2.1.0"),
    "- **First** change,\n  wrapped onto a second line.\n- Second change with `code`.\n\nA closing paragraph.",
  );
});

test("the tag matches with or without its v", () => {
  assert.equal(changelogSection(SAMPLE, "2.0.0"), "- The 2.0.0 change.");
  assert.equal(changelogSection(SAMPLE, "v2.0.0"), "- The 2.0.0 change.");
});

test("an entry ends at the rule that closes the list", () => {
  assert.equal(changelogSection(SAMPLE, "v1.9.0"), "- An entry without a date.");
});

test("a range entry belongs to no single release", () => {
  for (const tag of ["v2.0.1", "v2.0.9", "v2.0.5"]) {
    assert.equal(changelogSection(SAMPLE, tag), "", tag);
  }
});

test("a version is matched whole, never as a prefix", () => {
  assert.equal(changelogSection(SAMPLE, "v2.1"), "");
  assert.equal(changelogSection(SAMPLE, "v2.1.00"), "");
  assert.equal(changelogSection("## v1.2.30 — 2026-01-01\n\n- thirty\n", "v1.2.3"), "");
  assert.equal(changelogSection("## v1.2.3-rc.1\n\n- candidate\n", "v1.2.3"), "");
  assert.equal(changelogSection("## v1.2.3-rc.1\n\n- candidate\n", "v1.2.3-rc.1"), "- candidate");
});

test("nothing to find is an empty string, never an error", () => {
  assert.equal(changelogSection(SAMPLE, "v9.9.9"), "");
  assert.equal(changelogSection(SAMPLE, ""), "");
  assert.equal(changelogSection(SAMPLE, undefined), "");
  assert.equal(changelogSection(SAMPLE, "main"), "");
  assert.equal(changelogSection("", "v2.1.0"), "");
  assert.equal(changelogSection(undefined, "v2.1.0"), "");
  assert.equal(changelogSection("## v2.1.0 — 2026-12-01\n", "v2.1.0"), "");
});

test("CRLF files read the same", () => {
  assert.equal(changelogSection(SAMPLE.replace(/\n/g, "\r\n"), "v2.0.0"), "- The 2.0.0 change.");
});

function run(...args) {
  return execFileSync(process.execPath, [SCRIPT, ...args], { encoding: "utf8" });
}

test("the command exits 0 and prints nothing when there is no entry, file or tag", () => {
  // execFileSync throws on a non-zero exit, so returning at all is the exit-0 half.
  assert.equal(run("v0.0.1", CHANGELOG), "");
  assert.equal(run("v1.13.0", join(HERE, "no-such-changelog.md")), "");
  assert.equal(run(), "");
  assert.equal(run("refs/heads/main", CHANGELOG), "");
});

test("the command prints a real entry of the repository's CHANGELOG.md", () => {
  const out = run("v1.13.0", CHANGELOG);
  assert.match(out, /^- \*\*Notifications do not tell Apple, Google or Expo/);
  assert.ok(!out.includes("## "), "the next entry's heading must not leak in");
  assert.ok(out.endsWith("\n"));
});

test("the newest dated entry of CHANGELOG.md is one the release can read", () => {
  // Guards the heading shape the next release depends on: `## vX.Y.Z — YYYY-MM-DD`.
  const text = readFileSync(CHANGELOG, "utf8");
  const newest = text.match(/^## (v\d+\.\d+\.\d+) — \d{4}-\d{2}-\d{2}$/m);
  assert.ok(newest, "CHANGELOG.md has no dated entry");
  assert.notEqual(changelogSection(text, newest[1]), "");
});
