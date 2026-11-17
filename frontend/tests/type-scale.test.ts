/* The type scale by role (#1973): the same names as mobile's `type.*`, and no t-shirt size left behind. */

import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync, readdirSync } from "node:fs";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

import config from "../tailwind.config.js";

const SRC = fileURLToPath(new URL("../src", import.meta.url));
const ROLES = ["meta", "secondary", "body", "row", "heading", "title", "display"];

function sources(dir: string): string[] {
  return readdirSync(dir, { withFileTypes: true }).flatMap((e) => {
    const path = join(dir, e.name);
    if (e.isDirectory()) {
      return e.name === "locales" ? [] : sources(path);
    }
    return /\.(tsx?|css)$/.test(e.name) ? [path] : [];
  });
}

test("the scale is named by role, in rem", () => {
  const sizes = config.theme.fontSize as Record<string, [string, unknown]>;
  for (const role of ROLES) {
    assert.ok(sizes[role], `no text-${role}`);
  }
  for (const [name, [size]] of Object.entries(sizes)) {
    assert.match(size, /rem$/, `text-${name} must be rem so the font-size preference scales it`);
  }
});

test("no Tailwind t-shirt size is used (it would draw nothing)", () => {
  const stale = /(?<![\w-])text-(2xs|xs|sm|base|lg|[2-9]?xl)(?![\w-])/;
  const hits = sources(SRC).filter((file) => stale.test(readFileSync(file, "utf8")));
  assert.deepEqual(hits, []);
});
