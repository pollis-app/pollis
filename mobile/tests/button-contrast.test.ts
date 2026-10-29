/* A disabled Button stays legible (review #3/#4; review6 #9/#10), and no button has a border. */

import test from "node:test";
import assert from "node:assert/strict";

import { contrast, deriveTheme } from "../theme/derive.ts";
import { ACCENT_PRESETS, DEFAULT_BACKGROUND_HEX } from "../theme/accents.ts";
import {
  buttonColors,
  checkboxColors,
  controlDisabled,
  type ButtonVariant,
  type ControlSurface,
} from "../theme/button.ts";

const VARIANTS: ButtonVariant[] = ["primary", "secondary", "default", "danger", "subtle"];
const CONTROL_SURFACES: ControlSurface[] = ["base", "raised"];
// A transparent button's label sits on whatever surface holds it.
const SURFACES = ["bg", "panel", "raised", "high"] as const;

for (const preset of ACCENT_PRESETS) {
  const theme = deriveTheme(preset.c, DEFAULT_BACKGROUND_HEX);

  for (const surface of CONTROL_SURFACES) {
    for (const variant of VARIANTS) {
      for (const disabled of [true, false]) {
        const state = disabled ? "disabled" : "enabled";
        test(`${preset.n}: ${state} ${variant} on ${surface} label reaches 4.5:1 on its fill`, () => {
          const c = buttonColors(theme, variant, disabled, surface);
          const fills = c.fill ? [c.fill] : SURFACES.map((s) => theme[s]);
          for (const fill of fills) {
            const ratio = contrast(c.label, fill);
            assert.ok(
              ratio >= 4.5,
              `${state} ${variant}: label ${c.label} on ${fill} is ${ratio.toFixed(2)}:1`,
            );
          }
        });
      }
    }
  }

  test(`${preset.n}: no button variant or state has a border`, () => {
    for (const surface of CONTROL_SURFACES) {
      for (const variant of VARIANTS) {
        for (const disabled of [true, false]) {
          const c = buttonColors(theme, variant, disabled, surface) as Record<string, unknown>;
          for (const key of Object.keys(c)) {
            assert.ok(
              !/border/i.test(key),
              `${variant} (${disabled ? "disabled" : "enabled"}, ${surface}) carries ${key}`,
            );
          }
        }
      }
    }
    for (const surface of CONTROL_SURFACES) {
      const d = controlDisabled(theme, surface) as Record<string, unknown>;
      assert.deepEqual(Object.keys(d).sort(), ["fg", "fill"]);
    }
  });

  test(`${preset.n}: disabled differs from enabled by more than the fill`, () => {
    for (const surface of CONTROL_SURFACES) {
      for (const variant of VARIANTS) {
        const off = buttonColors(theme, variant, true, surface);
        const on = buttonColors(theme, variant, false, surface);
        // The label leaves the accent / onAccent tiers.
        assert.notEqual(off.label, on.label, `${variant} on ${surface}`);
        assert.notEqual(off.label, theme.accent, `${variant} on ${surface}`);
        assert.notEqual(off.label, theme.onAccent, `${variant} on ${surface}`);
        assert.equal(off.label, variant === "primary" ? theme.text : theme.dim);
      }
    }
  });

  // Review6 #10: a disabled primary must not look like a secondary.
  test(`${preset.n}: disabled primary keeps a primary identity`, () => {
    for (const surface of CONTROL_SURFACES) {
      const off = buttonColors(theme, "primary", true, surface);
      assert.equal(off.fill, theme.accentDisabled);
      for (const variant of ["secondary", "default", "danger"] as const) {
        for (const disabled of [true, false]) {
          const other = buttonColors(theme, variant, disabled, surface);
          assert.notEqual(off.fill, other.fill, `vs ${variant} ${disabled ? "disabled" : "enabled"}`);
          assert.notEqual(off.label, other.label, `vs ${variant} ${disabled ? "disabled" : "enabled"}`);
        }
      }
      const on = buttonColors(theme, "primary", false, surface);
      assert.notEqual(off.fill, on.fill);
      assert.notEqual(off.label, on.label);
      const behind = surface === "raised" ? theme.raised : theme.bg;
      const shape = contrast(off.fill as string, behind);
      assert.ok(shape >= 1.5, `disabled primary fill vs ${behind} is ${shape.toFixed(2)}:1`);
    }
  });

  // Review6 #9: ≥3:1 box on raised and high cards, ≥4.5:1 check glyph.
  test(`${preset.n}: checkbox box is visible on raised and high cards`, () => {
    for (const checked of [false, true]) {
      const c = checkboxColors(theme, checked) as Record<string, string>;
      assert.deepEqual(Object.keys(c).sort(), ["fill", "glyph"]);
      for (const surface of [theme.raised, theme.high]) {
        const ratio = contrast(c.fill, surface);
        assert.ok(ratio >= 3, `${checked ? "checked" : "unchecked"} box ${c.fill} on ${surface} is ${ratio.toFixed(2)}:1`);
      }
    }
    const on = checkboxColors(theme, true);
    const glyph = contrast(on.glyph, on.fill);
    assert.ok(glyph >= 4.5, `check glyph on accent is ${glyph.toFixed(2)}:1`);
    assert.notEqual(on.fill, checkboxColors(theme, false).fill);
  });

  test(`${preset.n}: the shared disabled look is legible on every surface`, () => {
    for (const surface of CONTROL_SURFACES) {
      const d = controlDisabled(theme, surface);
      // Transparent disabled controls put the dim glyph on the surface beneath.
      for (const behind of [d.fill, ...SURFACES.map((k) => theme[k])]) {
        const fg = contrast(d.fg, behind);
        assert.ok(fg >= 4.5, `disabled fg ${d.fg} on ${behind} is ${fg.toFixed(2)}:1`);
      }
    }
  });
}
