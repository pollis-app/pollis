// Unit tests for #1724's i18n coverage checks: the hardcoded-copy lint and its ratchet
// (scripts/i18n-hardcoded.mjs) and the surface registry (scripts/i18n-report.mjs).
// Needs the `typescript` package (frontend/ or mobile/ installed); run in frontend-check.yml.
// Run: node --test scripts/test-i18n-coverage.mjs

import { test } from "node:test";
import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { compare, scan, scanSource, tally } from "./i18n-hardcoded.mjs";
import { REQUIRED, coverage, validateSurfaces } from "./i18n-report.mjs";

const texts = (source) => scanSource(source).map((v) => `${v.kind}: ${v.text}`);

test("JSX text with a letter is copy; symbols, entities and whitespace are not", () => {
  assert.deepEqual(texts(`const A = () => <p>Sign in</p>;`), ["jsx-text: Sign in"]);
  assert.deepEqual(texts(`const A = () => <p>  /  &bull; &lt;{x}&gt; 42 · </p>;`), []);
  assert.deepEqual(texts(`const A = () => <p>\n   Wrapped\n   over lines\n</p>;`), ["jsx-text: Wrapped over lines"]);
});

test("the copy props are checked, other props are not", () => {
  assert.deepEqual(
    texts(`const A = () => <input placeholder="Email" title={"Hi"} aria-label='Close' alt="Logo" label="Name" className="text-sm" data-testid="email-input" />;`),
    ["prop placeholder: Email", "prop title: Hi", "prop aria-label: Close", "prop alt: Logo", "prop label: Name"],
  );
  assert.deepEqual(texts(`const A = () => <View accessibilityLabel="Back" accessibilityHint={\`Opens \${x}\`} testID="btn-back" />;`), [
    "prop accessibilityLabel: Back",
    "prop accessibilityHint: Opens {…}",
  ]);
});

test("t() calls and identifiers are never reported", () => {
  assert.deepEqual(texts(`const A = () => <p title={t("x.y")}>{t("a.b")}{name}</p>;`), []);
  assert.deepEqual(texts(`const A = () => <p placeholder={label ?? t("x")}>{ok ? t("a") : t("b")}</p>;`), []);
});

test("literal branches inside JSX expressions are copy", () => {
  assert.deepEqual(texts(`const A = () => <p>{busy ? "Saving…" : t("save")}</p>;`), ["jsx-expression: Saving…"]);
  assert.deepEqual(texts(`const A = () => <p>{error || "Something broke"}</p>;`), ["jsx-expression: Something broke"]);
  assert.deepEqual(texts(`const A = () => <p>{"Signed in as " + name}</p>;`), ["jsx-expression: Signed in as {…}"]);
});

test("error setters and native alerts are checked", () => {
  assert.deepEqual(texts(`setError("Invalid code");`), ["call setError: Invalid code"]);
  assert.deepEqual(texts(`setFetchError(\`Failed: \${e}\`);`), ["call setFetchError: Failed: {…}"]);
  assert.deepEqual(texts(`Alert.alert("Sign out?", "You will need your PIN");`), [
    "call Alert.alert: Sign out?",
    "call Alert.alert: You will need your PIN",
  ]);
  assert.deepEqual(texts(`setError(errorMessage(err, t("x")));`), []);
  assert.deepEqual(texts(`setStatus("error");`), []);
});

test("a new violation beyond the baseline fails; a fixed one is stale", () => {
  const v = (file, text, line) => ({ app: "desktop", file, line, kind: "jsx-text", text });
  const baseline = tally([v("a.tsx", "Hello", 1), v("b.tsx", "Bye", 2)]);
  const now = [v("a.tsx", "Hello", 1), v("a.tsx", "Hello", 9), v("c.tsx", "New", 3)];
  const { added, stale } = compare(now, baseline);
  assert.deepEqual(added.map((a) => `${a.file}:${a.line}`), ["a.tsx:9", "c.tsx:3"]);
  assert.deepEqual(stale.map((s) => `${s.file} ${s.text} ${s.count}→${s.have}`), ["b.tsx Bye 1→0"]);
  assert.deepEqual(compare(now, tally(now)), { added: [], stale: [] });
});

test("both apps' trees are scanned: a hardcoded string fails on desktop and on mobile", () => {
  const root = mkdtempSync(join(tmpdir(), "i18n-hardcoded-"));
  const put = (path, text) => {
    mkdirSync(join(root, path, ".."), { recursive: true });
    writeFileSync(join(root, path), text);
  };
  put("frontend/src/components/X.tsx", `export const X = () => <p>Desktop copy</p>;`);
  put("mobile/app/y.tsx", `export const Y = () => <Text accessibilityLabel="Mobile copy" />;`);
  put("mobile/components/z.tsx", `export const Z = () => <Text>{t("ok")}</Text>;`);
  put("mobile/hooks/w.tsx", `export const W = () => <Text>Pollis</Text>;`);
  put("mobile/app/dev/tool.tsx", `export const D = () => <Text>Dev only</Text>;`);
  const config = { allow: { Pollis: "brand", Unused: "x" }, ignore_files: { "mobile/app/dev/tool.tsx": "dev", "gone.tsx": "x" }, baseline: {} };
  const result = scan(config, root);
  assert.equal(result.files, 4);
  assert.deepEqual(result.violations.map((x) => `${x.app} ${x.file}: ${x.text}`), [
    "desktop frontend/src/components/X.tsx: Desktop copy",
    "mobile mobile/app/y.tsx: Mobile copy",
  ]);
  assert.deepEqual(result.unusedAllow, ["Unused"]);
  assert.deepEqual(result.missingIgnored, ["gone.tsx"]);
});

// A registry built here, not the committed one: the committed registry is checked by the sweep
// (`node scripts/i18n-check.mjs --report`), which never gates a merge (CLAUDE.md).
const fixture = () => ({
  surfaces: Object.fromEntries(
    REQUIRED.map((id) => [id, { title: id, decision: "english-only", reason: "test", paths: ["scripts"] }]),
  ),
});

test("a complete registry passes", () => {
  assert.deepEqual(validateSurfaces(fixture()), []);
});

test("the registry check fails on what it exists to catch", () => {
  assert.match(validateSurfaces({ surfaces: {} })[0], /empty registry/);

  const r = fixture();
  delete r.surfaces["email-otp"];
  r.surfaces.tui.paths = ["pollis-tui/no-such-dir"];
  Object.assign(r.surfaces["ds-errors"], { decision: "localize", status: "debt" });
  Object.assign(r.surfaces["push-ios"], { decision: "localize", status: "localized" });
  r.surfaces.website.status = "localized";
  r.surfaces.learn.decision = "maybe";
  const problems = validateSurfaces(r).join("\n");
  assert.match(problems, /required surface "email-otp" is missing/);
  assert.match(problems, /tui: path pollis-tui\/no-such-dir does not exist/);
  assert.match(problems, /ds-errors: status debt needs "debt"/);
  assert.match(problems, /push-ios: a localized surface needs "checked_by"/);
  assert.match(problems, /website: an english-only surface has no status/);
  assert.match(problems, /learn: decision must be one of/);
});

test("coverage counts a plural family once, and en is complete", () => {
  const cov = coverage();
  for (const { have, total } of Object.values(cov.en)) {
    assert.equal(have, total);
  }
  // zh has one plural form per family, uk four: the unit counts still line up with en's.
  assert.equal(cov.zh.errors.total, cov.uk.errors.total);
  assert.equal(cov.zh.errors.have, cov.uk.errors.have);
});
