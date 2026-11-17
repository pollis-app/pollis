/* The type scale by role (#1973): a component's `fontSize` reads `typeSize` / `type`, never a number. */
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync, readdirSync, statSync } from "node:fs";
import { dirname, join, relative } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = join(dirname(fileURLToPath(import.meta.url)), "..");

// Sizes that are not text roles: emoji and initials scaled to their box, the enrollment code, and
// the 11pt count badges (smaller than any text step on purpose).
const DELIBERATE: Record<string, RegExp[]> = {
  "app/(auth)/enrollment.tsx": [/fontSize: 34,/],
  "components/TabBar.tsx": [/fontSize: 11,/],
  "components/ui.tsx": [/fontSize: Math\.round\(dim \* 0\.38\)/, /fontSize: size === "sm" \? 11 : typeSize\.meta/],
  "components/chat/ReactionPills.tsx": [/fontSize: size - 2 /],
  "components/chat/MessageActionsSheet.tsx": [/fontSize: 24 /],
  "components/chat/Composer.tsx": [/fontSize: 20 /],
  "components/emoji/EmojiPickerSheet.tsx": [/fontSize: 26 /, /fontSize: 20 /],
  "components/emoji/CustomEmojiImage.tsx": [/fontSize: size \* 0\.7,/],
};

function walk(dir: string, out: string[] = []): string[] {
  for (const name of readdirSync(dir)) {
    const full = join(dir, name);
    if (statSync(full).isDirectory()) {
      walk(full, out);
    } else if (/\.tsx?$/.test(full)) {
      out.push(full);
    }
  }
  return out;
}

test("no inline fontSize outside the type scale, except the deliberate ones", () => {
  const stray: string[] = [];
  for (const file of ["app", "components"].flatMap((d) => walk(join(ROOT, d)))) {
    const rel = relative(ROOT, file);
    readFileSync(file, "utf8")
      .split("\n")
      .forEach((line, i) => {
        if (!/fontSize:/.test(line) || /fontSize: (typeSize|ty|type|v)\.[a-zA-Z.]+/.test(line)) {
          return;
        }
        if ((DELIBERATE[rel] ?? []).some((re) => re.test(line))) {
          return;
        }
        stray.push(`${rel}:${i + 1}: ${line.trim()}`);
      });
  }
  assert.deepEqual(stray, [], "use a `typeSize` role (theme/tokens.ts), or list a deliberate size here");
});

test("every deliberate size is still there (the list does not go stale)", () => {
  for (const [rel, patterns] of Object.entries(DELIBERATE)) {
    const src = readFileSync(join(ROOT, rel), "utf8");
    for (const re of patterns) {
      assert.match(src, re, `${rel} no longer has ${re}; drop it from DELIBERATE`);
    }
  }
});
