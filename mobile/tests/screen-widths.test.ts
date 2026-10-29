/* One centred column width per class of page on iPad (review7 N1); only auth steps pass `centered`. */
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync, readdirSync, statSync } from "node:fs";
import { dirname, join, relative } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = join(dirname(fileURLToPath(import.meta.url)), "..");

function walk(dir: string, out: string[] = []): string[] {
  for (const name of readdirSync(dir)) {
    const full = join(dir, name);
    if (statSync(full).isDirectory()) {
      walk(full, out);
    } else if (full.endsWith(".tsx")) {
      out.push(full);
    }
  }
  return out;
}

const routes = walk(join(ROOT, "app")).map((f) => ({
  rel: relative(ROOT, f),
  src: readFileSync(f, "utf8"),
}));

test("only auth steps use the narrower centred column", () => {
  for (const { rel, src } of routes) {
    if (rel.startsWith("app/(auth)/")) {
      continue;
    }
    assert.doesNotMatch(src, /<Screen\b[^>]*\bcentered\b/, `${rel} must use the default Screen width`);
  }
});

test("route files do not hard-code a page column width", () => {
  for (const { rel, src } of routes) {
    assert.doesNotMatch(
      src,
      /maxWidth:\s*(layout\.\w+|5\d\d|6\d\d)/,
      `${rel} sets its own page max width; use <Screen>`,
    );
  }
});
