/* Lucide icons come in one deep import each, never through the barrel (#2187): Metro does not
 * tree-shake, so a value import from "lucide-react-native" bundles every icon (~1,800 modules)
 * and runs each one's factory at first import. */

import test from "node:test";
import assert from "node:assert/strict";
import { existsSync, readdirSync, readFileSync, statSync } from "node:fs";
import { join, relative } from "node:path";
import { barrelFiles } from "../scripts/lucide-deep-imports.mjs";

const MOBILE = new URL("..", import.meta.url).pathname;
const SKIP = new Set(["node_modules", "android", "ios", ".expo", "dist", "build", "tests", "scripts", ".maestro"]);

function sources(dir: string, out: string[] = []): string[] {
  for (const entry of readdirSync(dir)) {
    if (SKIP.has(entry) || entry.startsWith(".")) {
      continue;
    }
    const p = join(dir, entry);
    if (statSync(p).isDirectory()) {
      sources(p, out);
    } else if (/\.(ts|tsx|js|jsx|mjs)$/.test(entry)) {
      out.push(p);
    }
  }
  return out;
}

test("no app module takes a value import from the lucide-react-native barrel", () => {
  const offenders: string[] = [];
  for (const file of sources(MOBILE)) {
    const text = readFileSync(file, "utf8");
    for (const m of text.matchAll(/import\s+(type\s+)?([^;]*?)\s+from\s+["']lucide-react-native(?:\/icons)?["']/g)) {
      if (!m[1]) {
        offenders.push(`${relative(MOBILE, file)}: ${m[0]}`);
      }
    }
    // A require of the barrel is the same bundle.
    if (/require\(\s*["']lucide-react-native["']\s*\)/.test(text)) {
      offenders.push(`${relative(MOBILE, file)}: require("lucide-react-native")`);
    }
  }
  assert.deepEqual(offenders, []);
});

test("every deep icon import names the icon's canonical file", () => {
  const barrel = readFileSync(
    join(MOBILE, "node_modules/lucide-react-native/dist/esm/lucide-react-native.mjs"),
    "utf8",
  );
  const fileOf = barrelFiles(barrel);
  const icons = readFileSync(join(MOBILE, "components/icons.tsx"), "utf8");
  const deep = [...icons.matchAll(/^import (\w+) from "lucide-react-native\/icons\/([a-z0-9-]+)";$/gm)];
  assert.ok(deep.length > 50, "components/icons.tsx imports its icons one by one");
  for (const [, name, file] of deep) {
    assert.equal(fileOf.get(name), file, `${name} lives in icons/${fileOf.get(name)}, not icons/${file}`);
    assert.ok(
      existsSync(join(MOBILE, `node_modules/lucide-react-native/dist/esm/icons/${file}.mjs`)),
      `icons/${file} exists`,
    );
  }
});
