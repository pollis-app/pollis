/* Every photo-library pick asks iOS for JPEG, not the original HEIC, which desktop webviews on Linux and Windows cannot draw. */
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync, readdirSync, statSync } from "node:fs";
import { dirname, join, relative } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");

function sources(dir: string): string[] {
  return readdirSync(dir).flatMap((name) => {
    const path = join(dir, name);
    if (statSync(path).isDirectory()) {
      return sources(path);
    }
    return /\.tsx?$/.test(name) ? [path] : [];
  });
}

test("every launchImageLibraryAsync call uses COMPATIBLE_PICK", () => {
  const calls: string[] = [];
  for (const file of ["app", "components", "lib", "hooks"].flatMap((d) => sources(join(root, d)))) {
    const text = readFileSync(file, "utf8");
    if (!text.includes("launchImageLibraryAsync(")) {
      continue;
    }
    calls.push(file);
    assert.ok(text.includes("COMPATIBLE_PICK"), `${relative(root, file)} picks photos without COMPATIBLE_PICK`);
  }
  assert.ok(calls.length >= 4, `expected the chat, Vault, emoji and profile pickers, found ${calls.length}`);
});
