// A platform-only accessibility role must be chosen by `Platform.OS`: Android crashes on "tabbar".
import { test } from "node:test";
import assert from "node:assert/strict";
import { readdirSync, readFileSync, statSync } from "node:fs";
import { join } from "node:path";

const IOS_ONLY_ROLES = ["tabbar"];
const ROOTS = ["app", "components"];

function sourceFiles(dir: string): string[] {
  const out: string[] = [];
  for (const name of readdirSync(dir)) {
    const path = join(dir, name);
    if (statSync(path).isDirectory()) {
      out.push(...sourceFiles(path));
    } else if (/\.tsx?$/.test(name)) {
      out.push(path);
    }
  }
  return out;
}

test("no iOS-only accessibility role is written as a literal", () => {
  const root = join(import.meta.dirname, "..");
  const offenders: string[] = [];
  for (const dir of ROOTS) {
    for (const file of sourceFiles(join(root, dir))) {
      const text = readFileSync(file, "utf8");
      for (const role of IOS_ONLY_ROLES) {
        const literal = new RegExp(`accessibilityRole=\\{?["']${role}["']\\}?`);
        if (literal.test(text)) {
          offenders.push(`${file}: ${role}`);
        }
      }
    }
  }
  assert.deepEqual(offenders, []);
});
