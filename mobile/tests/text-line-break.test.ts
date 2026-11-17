/* Hangul wraps at word boundaries on iOS (#1941): every Text is components/Text.tsx; Maestro cannot see line breaks. */
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync, readdirSync, statSync } from "node:fs";
import { dirname, join, relative } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = join(dirname(fileURLToPath(import.meta.url)), "..");
const read = (rel: string) => readFileSync(join(ROOT, rel), "utf8");

function sources(): string[] {
  const out: string[] = [];
  const walk = (dir: string) => {
    for (const name of readdirSync(dir)) {
      const full = join(dir, name);
      if (statSync(full).isDirectory()) {
        walk(full);
      } else if (/\.tsx?$/.test(name)) {
        out.push(relative(ROOT, full));
      }
    }
  };
  for (const dir of ["app", "components", "hooks", "lib", "theme"]) {
    walk(join(ROOT, dir));
  }
  return out;
}

test("the app's Text keeps Hangul words whole on iOS, and a caller can still override it", () => {
  const src = read("components/Text.tsx");
  assert.match(src, /<RNText lineBreakStrategyIOS="hangul-word" \{\.\.\.props\} \/>/);
});

test("nothing takes Text from react-native except components/Text.tsx", () => {
  const offenders = sources().filter((file) => {
    if (file === join("components", "Text.tsx")) {
      return false;
    }
    const src = read(file);
    for (const m of src.matchAll(/import\s*(?:type\s*)?\{([^}]*)\}\s*from\s*["']react-native["']/g)) {
      const names = m[1].split(",").map((n) => n.trim().replace(/^type\s+/, ""));
      if (names.some((n) => n === "Text" || n.startsWith("Text as "))) {
        return true;
      }
    }
    return /\bRN\.Text\b|require\(["']react-native["']\)\.Text/.test(src);
  });
  assert.deepEqual(offenders, [], "import Text from components/Text instead");
});
