/* Admin name-chip fills (#1488) stay readable under `text-ink` and match the dark-theme name colour. */

import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

import { getUsernameChipColor, getUsernameColor } from "../src/utils/usernameColor.ts";

type Rgb = [number, number, number];

function hslToRgb(h: number, s: number, l: number): Rgb {
  const sl = s / 100;
  const ll = l / 100;
  const a = sl * Math.min(ll, 1 - ll);
  const f = (n: number) => {
    const k = (n + h / 30) % 12;
    return ll - a * Math.max(Math.min(k - 3, 9 - k, 1), -1);
  };
  return [f(0), f(8), f(4)];
}

function parseHsl(css: string): Rgb {
  const m = /hsl\((\d+(?:\.\d+)?) (\d+(?:\.\d+)?)% (\d+(?:\.\d+)?)%\)/.exec(css);
  assert.ok(m, `not an hsl() colour: ${css}`);
  return hslToRgb(Number(m[1]), Number(m[2]), Number(m[3]));
}

function luminance([r, g, b]: Rgb): number {
  const lin = (c: number) => (c <= 0.03928 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4);
  return 0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(b);
}

function contrast(a: Rgb, b: Rgb): number {
  const x = luminance(a);
  const y = luminance(b);
  return (Math.max(x, y) + 0.05) / (Math.min(x, y) + 0.05);
}

const css = readFileSync(new URL("../src/index.css", import.meta.url), "utf8");
const inkDecl = /--c-ink:\s*([^;]+);/.exec(css);

test("--c-ink is a fixed dark colour, not a theme-relative one", () => {
  assert.ok(inkDecl, "--c-ink must be declared in index.css");
  assert.doesNotMatch(inkDecl[1], /var\(/);
  assert.ok(luminance(parseHsl(inkDecl[1].trim())) < 0.01);
});

test("every admin chip fill keeps dark ink at WCAG AA (4.5:1) or better", () => {
  const ink = parseHsl(inkDecl![1].trim());
  let worst = Infinity;
  let worstKey = "";
  // Enough keys to land on every one of the 360 hue buckets many times over.
  for (let i = 0; i < 20000; i++) {
    const key = `user_${i}`;
    const ratio = contrast(parseHsl(getUsernameChipColor(key)), ink);
    if (ratio < worst) {
      worst = ratio;
      worstKey = key;
    }
  }
  assert.ok(worst >= 4.5, `${worstKey} chip contrast ${worst.toFixed(2)}:1`);
});

test("the chip fill is the user's own name colour, and differs between users", () => {
  for (const key of ["dana", "eve", "zed", "u_123"]) {
    assert.equal(getUsernameChipColor(key), getUsernameColor(key, false));
  }
  assert.notEqual(getUsernameChipColor("dana"), getUsernameChipColor("eve"));
});
