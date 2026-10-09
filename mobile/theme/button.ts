// Button colours per variant and state, pure (no react-native) so
// tests/button-contrast.test.ts can check every pair against the derived
// theme for every accent preset.
//
// No button has a border, in any state: shape comes from the fill. Primary is
// an accent fill with a dark (onAccent) label; secondary / default / danger
// are the inverse, a dark surface fill with an accent label; subtle has no
// fill. On a `raised` card the dark fill steps up to `high` so the button
// still reads as a shape.
//
// Disabled is drawn with SOLID derived tokens, never `opacity` on the whole
// button: on Android a translucent parent fades each child separately (no
// offscreen compositing), so the label blended into the faded fill and fell
// far below 4.5:1 (#3/#4). Disabled is told apart from enabled by more than
// hue: the label leaves the accent / onAccent tiers, and the control is
// announced and skipped as disabled. Secondary / default / danger / subtle
// share the plain surface fill + `dim` label. Disabled PRIMARY keeps its own
// identity (review6 #10) so it never reads as a secondary button: a dimmed
// accent fill (`accentDisabled`: 40% accent over the background)
// with a light `text` label. A dark label on that fill cannot reach 4.5:1 for
// every preset (it tops out near 3:1), while `text` stays above 5.9:1.

import type { Theme } from "./derive";

export type ButtonVariant = "primary" | "secondary" | "subtle" | "danger" | "default";

// The surface a control sits on: the screen (`bg`/`panel`) or a `raised`
// card or group, where a `raised` fill would vanish.
export type ControlSurface = "base" | "raised";

export type ButtonColors = {
  // `undefined` = transparent (the label then sits on whatever is behind).
  fill: string | undefined;
  // Fill while pressed (enabled only; enabled buttons also dim slightly on
  // press — transient, so its effect on the label doesn't matter).
  pressedFill: string | undefined;
  label: string;
};

/** The dark fill a non-primary control uses on `surface`. */
export function controlFill(theme: Theme, surface: ControlSurface = "base"): string {
  return surface === "raised" ? theme.high : theme.raised;
}

/**
 * The one disabled look every control shares (Button, IconButton, Chip,
 * ListRow, Toggle, PinPad, the composer's attach): a solid surface fill
 * (`raised`, or `high` on a raised card) and `dim` for the label, glyph or
 * knob. No border; nothing depends on an opacity fade.
 */
export function controlDisabled(
  theme: Theme,
  surface: ControlSurface = "base",
): {
  fill: string;
  fg: string;
} {
  return { fill: controlFill(theme, surface), fg: theme.dim };
}

/**
 * The fill + label of a disabled PRIMARY button: the accent dimmed toward
 * the background, with a light `text` label. Same on every surface.
 */
export function disabledPrimary(theme: Theme): { fill: string; fg: string } {
  return { fill: theme.accentDisabled, fg: theme.text };
}

/**
 * A checkbox box (review6 #9). No border in any state. Unchecked: a light
 * `muted` square that reads as a shape on a raised or high card (≥3:1 vs
 * both). Checked: an accent fill with a dark onAccent check glyph — the
 * glyph, not the hue, carries the state.
 */
export function checkboxColors(
  theme: Theme,
  checked: boolean,
): { fill: string; glyph: string } {
  if (checked) {
    return { fill: theme.accent, glyph: theme.onAccent };
  }
  return { fill: theme.muted, glyph: theme.onAccent };
}

/** The colours a Button paints with, from the current theme. */
export function buttonColors(
  theme: Theme,
  variant: ButtonVariant,
  disabled: boolean,
  surface: ControlSurface = "base",
): ButtonColors {
  if (disabled && variant === "primary") {
    const d = disabledPrimary(theme);
    return { fill: d.fill, pressedFill: d.fill, label: d.fg };
  }
  if (disabled) {
    const d = controlDisabled(theme, surface);
    return { fill: d.fill, pressedFill: d.fill, label: d.fg };
  }
  if (variant === "primary") {
    return { fill: theme.accent, pressedFill: theme.accent, label: theme.onAccent };
  }
  if (variant === "subtle") {
    return { fill: undefined, pressedFill: controlFill(theme, surface), label: theme.accent };
  }
  const fill = controlFill(theme, surface);
  return {
    fill,
    pressedFill: surface === "raised" ? theme.accentSoft : theme.high,
    label: theme.accent,
  };
}
