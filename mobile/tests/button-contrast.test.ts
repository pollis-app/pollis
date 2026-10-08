/*
 * A disabled Button stays legible (review defects #3/#4).
 *
 * Disabled used to be `opacity: 0.45` on the whole button. On Android that
 * fades the fill and the label separately, so the label blended into the
 * faded fill (faint tan on muted amber). Now `theme/button.ts` paints disabled
 * with solid derived tokens; this pins every label/fill pair at ≥ 4.5:1 for
 * every accent preset, and that disabled differs from enabled by more than
 * colour (a dashed border).
 *
 * The same disabled look (`controlDisabled`) is shared by IconButton, Chip,
 * ListRow, Toggle, ToggleRow, the PIN pad and the composer's attach button,
 * so it is pinned on every surface those can sit on.
 *
 *   cd mobile && pnpm test
 */

import test from "node:test";
import assert from "node:assert/strict";

import { contrast, deriveTheme } from "../theme/derive.ts";
import { ACCENT_PRESETS, DEFAULT_BACKGROUND_HEX } from "../theme/accents.ts";
import { buttonColors, controlDisabled, type ButtonVariant } from "../theme/button.ts";

const VARIANTS: ButtonVariant[] = ["primary", "secondary", "default", "danger", "subtle"];
// A transparent button's label sits on whatever surface holds it.
const SURFACES = ["bg", "panel", "raised", "high"] as const;

for (const preset of ACCENT_PRESETS) {
  const theme = deriveTheme(preset.c, DEFAULT_BACKGROUND_HEX);

  for (const variant of VARIANTS) {
    for (const disabled of [true, false]) {
      const state = disabled ? "disabled" : "enabled";
      test(`${preset.n}: ${state} ${variant} label reaches 4.5:1 on its fill`, () => {
        const c = buttonColors(theme, variant, disabled);
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

  test(`${preset.n}: disabled is not told apart by colour alone`, () => {
    for (const variant of VARIANTS) {
      const off = buttonColors(theme, variant, true);
      const on = buttonColors(theme, variant, false);
      assert.equal(off.borderStyle, "dashed", `${variant} disabled needs a dashed border`);
      assert.equal(on.borderStyle, "solid");
      assert.ok(off.border, `${variant} disabled needs a visible border`);
      // The dashed border must itself be visible against the fill (1.4.11).
      const behind = off.fill ?? theme.bg;
      assert.ok(contrast(off.border!, behind) >= 3, `${variant} disabled border vs fill`);
    }
  });

  test(`${preset.n}: the shared disabled look is legible on every surface`, () => {
    const d = controlDisabled(theme);
    assert.equal(d.borderStyle, "dashed");
    // Transparent disabled controls (IconButton, PIN keys, outline chips)
    // put the dim glyph straight on the surface beneath.
    for (const surface of [d.fill, ...SURFACES.map((k) => theme[k])]) {
      const fg = contrast(d.fg, surface);
      assert.ok(fg >= 4.5, `disabled fg ${d.fg} on ${surface} is ${fg.toFixed(2)}:1`);
    }
    const ring = contrast(d.border, d.fill);
    assert.ok(ring >= 3, `disabled border on its fill is ${ring.toFixed(2)}:1`);
  });
}
