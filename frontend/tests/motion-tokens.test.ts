/* The motion limits (#2125, docs/design/README.md → Motion): nothing over 240 ms, no overshoot past
   8 %, one curve, and every movement in motion.css has a reduced-motion answer. */

import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync, readdirSync, statSync } from "node:fs";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

import { MOTION, cssCurve } from "../../design/theme.ts";
import { motionCssVars } from "../src/utils/motion.ts";

const SRC = fileURLToPath(new URL("../src", import.meta.url));
const css = readFileSync(join(SRC, "components/ui/motion.css"), "utf8");

test("no timing runs past slowMs", () => {
  for (const ms of [MOTION.fastMs, MOTION.baseMs, MOTION.slowMs, MOTION.exitMs]) {
    assert.ok(ms <= 240, `${ms} ms`);
  }
  assert.ok(MOTION.fastMs < MOTION.baseMs && MOTION.baseMs < MOTION.slowMs);
});

test("no overshoot past 8 %", () => {
  assert.ok(MOTION.popPeakScale - 1 <= MOTION.maxOvershoot + 1e-9);
  assert.ok(MOTION.bumpScale - 1 <= MOTION.maxOvershoot + 1e-9);
  assert.ok(MOTION.maxOvershoot <= 0.08);
  // The curve itself never overshoots: both y control points stay within [0, 1].
  assert.ok(MOTION.easeOut[1] <= 1 && MOTION.easeOut[3] <= 1);
});

test("the CSS variables come from the shared tokens", () => {
  const vars = motionCssVars();
  assert.equal(vars["--motion-base"], `${MOTION.baseMs}ms`);
  assert.equal(vars["--motion-ease-out"], cssCurve(MOTION.easeOut));
  assert.equal(vars["--motion-pop-peak"], String(MOTION.popPeakScale));
});

test("motion.css writes no timing or curve of its own", () => {
  const body = css.replace(/\/\*[\s\S]*?\*\//g, "");
  // The status blink is the one long loop, and it stops under reduced motion.
  const durations = [...body.matchAll(/\b(\d+(?:\.\d+)?)(ms|s)\b/g)].map((m) => m[0]);
  assert.deepEqual(durations, ["2s"]);
  assert.doesNotMatch(body, /cubic-bezier\(/);
  assert.doesNotMatch(body, /box-shadow|drop-shadow|text-shadow/);
});

test("every moving class has a reduced-motion answer", () => {
  const reduced = css.slice(css.indexOf("@media (prefers-reduced-motion: reduce)"));
  for (const cls of [
    "motion-rise",
    "motion-pop",
    "motion-roll",
    "motion-bump",
    "motion-bump-again",
    "motion-popover-up",
    "motion-popover-down",
    "motion-hover-grow",
    "motion-chevron",
    "motion-fold",
    "status-bar-blink",
  ]) {
    assert.match(reduced, new RegExp(`\\.${cls}\\b`), cls);
  }
  assert.match(reduced, /scroll-behavior: auto !important/);
});

function walk(dir: string, out: string[] = []): string[] {
  for (const name of readdirSync(dir)) {
    const p = join(dir, name);
    if (statSync(p).isDirectory()) {
      walk(p, out);
    } else if (/\.(tsx?|css)$/.test(name)) {
      out.push(p);
    }
  }
  return out;
}

test("smooth scrolls go through scrollBehavior(), so reduced motion jumps", () => {
  for (const file of walk(SRC)) {
    const text = readFileSync(file, "utf8");
    assert.doesNotMatch(text, /behavior: "smooth"/, file);
  }
});
