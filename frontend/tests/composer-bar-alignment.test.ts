/*
 * The bars along the bottom of the window line up with the composer.
 *
 * `--composer-h` is the height of the composer's INPUT ROW. `ChatInput` draws
 * its top hairline on the root *above* that row, so the composer's real
 * footprint is `--composer-h` + 1px.
 *
 * The sidebar Close bar, the right-panel Close bar and refined's profile strip
 * each draw their own `border-t` on the SAME element that carries the height —
 * and under `box-sizing: border-box` that hairline comes out of the height
 * rather than adding to it. All three were therefore exactly 1px shorter than
 * the composer they sit beside, which is visible as a broken seam where the
 * sidebar meets the chat pane.
 *
 * `--composer-flush-h` is the same height with the hairline added back. The
 * rule this test enforces is the one that is easy to get wrong at a glance,
 * because both class names look equally plausible at the call site:
 *
 *   borders itself  → min-h-composer-flush
 *   border on an ancestor → min-h-composer
 *
 *   node --test frontend/tests/
 */

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
