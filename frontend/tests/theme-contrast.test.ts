/*
 * Every theme the desktop can paint meets WCAG AA (design/theme.ts): both skins, every accent
 * preset, every background preset. Mobile runs the same check on its own surfaces.
 */

import test from "node:test";
import assert from "node:assert/strict";

import {
  ACCENT_PRESETS,
  BACKGROUND_PRESETS,
  contrast,
  deriveTokens,
  flatten,
  type ThemeVariant,
} from "../../design/theme.ts";

const VARIANTS: ThemeVariant[] = ["modern", "terminal"];
const SURFACES = ["bg", "surface", "surfaceRaised", "surfaceHigh"] as const;
const TEXT = ["text", "textDim", "textMuted", "accent", "danger"] as const;

for (const variant of VARIANTS) {
  for (const bg of BACKGROUND_PRESETS) {
    for (const accent of ACCENT_PRESETS) {
      const t = deriveTokens(accent.c, bg.c, variant);
      const name = `${variant} · ${accent.n} on ${bg.n}`;

      test(`${name}: text reaches 4.5:1 on every surface`, () => {
        for (const fg of TEXT) {
          for (const surface of SURFACES) {
            const ratio = contrast(t[fg], t[surface]);
            assert.ok(ratio >= 4.5, `${fg} ${t[fg]} on ${surface} ${t[surface]} is ${ratio.toFixed(2)}:1`);
          }
        }
      });

      test(`${name}: a control's edge reaches 3:1 on every surface`, () => {
        for (const surface of SURFACES) {
          const edge = flatten(t.borderStrong, t[surface]);
          const ratio = contrast(edge, t[surface]);
          assert.ok(ratio >= 3, `borderStrong on ${surface} is ${ratio.toFixed(2)}:1`);
        }
      });

      test(`${name}: ink on an accent or danger fill reaches 4.5:1`, () => {
        assert.ok(contrast(t.onAccent, t.accent) >= 4.5, `onAccent on accent ${contrast(t.onAccent, t.accent).toFixed(2)}:1`);
        assert.ok(contrast(t.onDanger, t.danger) >= 4.5, `onDanger on danger ${contrast(t.onDanger, t.danger).toFixed(2)}:1`);
      });
    }
  }
}

test("one red: danger is the same in both skins and never an accent", () => {
  for (const accent of ACCENT_PRESETS) {
    const modern = deriveTokens(accent.c, BACKGROUND_PRESETS[0].c, "modern");
    const terminal = deriveTokens(accent.c, BACKGROUND_PRESETS[0].c, "terminal");
    assert.equal(modern.danger, terminal.danger);
    assert.notEqual(modern.danger, modern.accent);
  }
});

test("Modern on desktop is mobile's palette: same recipe, same values", () => {
  const t = deriveTokens("#fabf5a", "#0a0907", "modern");
  // Pinned to mobile's mockup ramp (mobile/tests/theme-contrast.test.ts).
  assert.equal(t.bg, "#0a0907");
  assert.equal(t.text, "#f4e7d1");
  assert.equal(t.surface, "#1c1811");
  assert.equal(t.onAccent, "#0a0907");
});

test("a light background flips the ramp and stays legible", () => {
  for (const variant of VARIANTS) {
    const t = deriveTokens("#b5651d", "#f5f1ea", variant);
    assert.equal(t.isLight, true);
    for (const surface of SURFACES) {
      assert.ok(contrast(t.text, t[surface]) >= 4.5, `${variant} text on ${surface}`);
      assert.ok(contrast(t.danger, t[surface]) >= 4.5, `${variant} danger on ${surface}`);
    }
  }
});
