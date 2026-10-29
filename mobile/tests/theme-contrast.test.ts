/* The derived theme meets WCAG AA for every accent the app offers. */

import test from "node:test";
import assert from "node:assert/strict";

import { contrast, deriveTheme } from "../theme/derive.ts";
import { ACCENT_PRESETS, DEFAULT_BACKGROUND_HEX } from "../theme/accents.ts";

const SURFACES = ["bg", "panel", "raised", "high"] as const;
const TEXT_TOKENS = ["text", "dim", "muted", "accent"] as const;

for (const preset of ACCENT_PRESETS) {
  const theme = deriveTheme(preset.c, DEFAULT_BACKGROUND_HEX);

  test(`${preset.n}: text tokens reach 4.5:1 on every surface`, () => {
    for (const fg of TEXT_TOKENS) {
      for (const surface of SURFACES) {
        const ratio = contrast(theme[fg], theme[surface]);
        assert.ok(
          ratio >= 4.5,
          `${fg} ${theme[fg]} on ${surface} ${theme[surface]} is ${ratio.toFixed(2)}:1`,
        );
      }
    }
  });

  test(`${preset.n}: control edge reaches 3:1 against raised`, () => {
    const ratio = contrast(theme.edge, theme.raised);
    assert.ok(ratio >= 3, `edge ${theme.edge} on raised ${theme.raised} is ${ratio.toFixed(2)}:1`);
  });

  test(`${preset.n}: text on an accent fill reaches 4.5:1`, () => {
    const ratio = contrast(theme.onAccent, theme.accent);
    assert.ok(ratio >= 4.5, `onAccent on accent is ${ratio.toFixed(2)}:1`);
  });
}

test("the default theme lands on the mockup ramp", () => {
  const theme = deriveTheme("#fabf5a", "#0a0907");
  assert.equal(theme.bg, "#0a0907");
  assert.equal(theme.accent, "#fabf5a");
  assert.equal(theme.onAccent, "#0a0907");
  const lum = (k: "bg" | "panel" | "raised" | "high") => contrast(theme[k], "#000000");
  assert.ok(lum("bg") < lum("panel") && lum("panel") < lum("raised") && lum("raised") < lum("high"));
});

test("a different background re-derives the neutrals", () => {
  const dark = deriveTheme("#fabf5a", "#0a0907");
  const other = deriveTheme("#fabf5a", "#101820");
  for (const key of ["text", "dim", "muted", "panel", "raised", "high", "edge", "onAccent"] as const) {
    assert.notEqual(dark[key], other[key], `${key} ignored the background`);
  }
});

test("there is no third hue: danger is the accent", () => {
  for (const preset of ACCENT_PRESETS) {
    const theme = deriveTheme(preset.c, DEFAULT_BACKGROUND_HEX);
    assert.equal(theme.danger, theme.accent);
  }
});
