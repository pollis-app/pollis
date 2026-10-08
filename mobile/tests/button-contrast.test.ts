/*
 * A disabled Button stays legible (review defects #3/#4), and no button has
 * a border.
 *
 * Disabled used to be `opacity: 0.45` on the whole button. On Android that
 * fades the fill and the label separately, so the label blended into the
 * faded fill (faint tan on muted amber). Now `theme/button.ts` paints disabled
 * with solid derived tokens; this pins every label/fill pair at ≥ 4.5:1 for
 * every accent preset, every variant and state, on both surfaces a button can
 * sit on (the screen, or a raised card where its fill steps up to `high`).
 *
 * Buttons are borderless in every state (the shape is the fill), so the
 * colours carry no border at all.
 *
 * The same disabled look (`controlDisabled`) is shared by IconButton, Chip,
 * ListRow, Toggle, the PIN pad and the composer's attach button, so it is
 * pinned on every surface those can sit on.
 *
 *   cd mobile && pnpm test
 */

import test from "node:test";
import assert from "node:assert/strict";

import { contrast, deriveTheme } from "../theme/derive.ts";
import { ACCENT_PRESETS, DEFAULT_BACKGROUND_HEX } from "../theme/accents.ts";
import {
  buttonColors,
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
        // The label leaves the accent / onAccent tiers for dim.
        assert.notEqual(off.label, on.label, `${variant} on ${surface}`);
        assert.equal(off.label, theme.dim);
      }
    }
  });

  test(`${preset.n}: the shared disabled look is legible on every surface`, () => {
    for (const surface of CONTROL_SURFACES) {
      const d = controlDisabled(theme, surface);
      // Transparent disabled controls (the PIN pad's delete key) put the dim
      // glyph straight on the surface beneath.
      for (const behind of [d.fill, ...SURFACES.map((k) => theme[k])]) {
        const fg = contrast(d.fg, behind);
        assert.ok(fg >= 4.5, `disabled fg ${d.fg} on ${behind} is ${fg.toFixed(2)}:1`);
      }
    }
  });
}
