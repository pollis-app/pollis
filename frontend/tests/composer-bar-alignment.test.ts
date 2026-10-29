/* Bottom bars line up with the composer: a self-bordered bar uses `min-h-composer-flush` (`--composer-h` + 1px), else `min-h-composer`. */

import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync, readdirSync, statSync } from "node:fs";
import { resolve, dirname, join, relative } from "node:path";
import { fileURLToPath } from "node:url";

const SRC = resolve(dirname(fileURLToPath(import.meta.url)), "../src");

function sourceFiles(dir: string): string[] {
  const out: string[] = [];
  for (const entry of readdirSync(dir)) {
    const full = join(dir, entry);
    if (statSync(full).isDirectory()) {
      out.push(...sourceFiles(full));
    } else if (/\.tsx?$/.test(entry)) {
      out.push(full);
    }
  }
  return out;
}

/** Every `className` value in a file, whether quoted or a template literal. */
function classAttributes(source: string): string[] {
  const out: string[] = [];
  const re = /className=(?:"([^"]*)"|\{`([^`]*)`\}|\{"([^"]*)"\})/g;
  let m: RegExpExecArray | null;
  while ((m = re.exec(source)) !== null) {
    out.push(m[1] ?? m[2] ?? m[3] ?? "");
  }
  return out;
}

const HAS_PLAIN = /(?:^|\s)min-h-composer(?![\w-])/;
const HAS_FLUSH = /(?:^|\s)min-h-composer-flush(?![\w-])/;
const HAS_BORDER_TOP = /(?:^|\s)border-t(?![\w-])/;

test("a bar that draws its own top hairline uses the flush height", () => {
  const offenders: string[] = [];
  for (const file of sourceFiles(SRC)) {
    for (const cls of classAttributes(readFileSync(file, "utf8"))) {
      if (HAS_PLAIN.test(cls) && HAS_BORDER_TOP.test(cls)) {
        offenders.push(`${relative(SRC, file)}: ${cls.trim().slice(0, 120)}`);
      }
    }
  }
  assert.deepEqual(
    offenders,
    [],
    "these carry `border-t` on the same element as `min-h-composer`, so the " +
      "hairline is eaten out of the height and the bar renders 1px shorter " +
      "than the composer — use `min-h-composer-flush`:\n  " +
      offenders.join("\n  "),
  );
});

test("the flush height is only used by bars that do draw their own hairline", () => {
  const offenders: string[] = [];
  for (const file of sourceFiles(SRC)) {
    for (const cls of classAttributes(readFileSync(file, "utf8"))) {
      if (HAS_FLUSH.test(cls) && !HAS_BORDER_TOP.test(cls)) {
        offenders.push(`${relative(SRC, file)}: ${cls.trim().slice(0, 120)}`);
      }
    }
  }
  assert.deepEqual(
    offenders,
    [],
    "these use `min-h-composer-flush` without a `border-t` to absorb, so they " +
      "render 1px TALLER than the composer — use `min-h-composer`:\n  " +
      offenders.join("\n  "),
  );
});

test("the flush token is defined as the composer height plus its hairline", () => {
  const css = readFileSync(resolve(SRC, "index.css"), "utf8");
  const match = css.match(/--composer-flush-h:\s*([^;]+);/);
  assert.ok(match, "--composer-flush-h is gone from index.css");
  assert.equal(
    match[1].replace(/\s+/g, " ").trim(),
    "calc(var(--composer-h) + 1px)",
    "the flush height must stay derived from --composer-h, or the two drift",
  );

  const tw = readFileSync(resolve(SRC, "../tailwind.config.js"), "utf8");
  assert.match(
    tw,
    /'composer-flush':\s*'var\(--composer-flush-h\)'/,
    "the theme no longer exposes composer-flush, so min-h-composer-flush is inert",
  );
});
