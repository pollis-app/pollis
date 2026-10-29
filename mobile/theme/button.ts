// Button colours per variant and state, pure. Disabled uses SOLID tokens, never opacity: Android fades children separately (#3/#4).

import type { Theme } from "./derive";

export type ButtonVariant = "primary" | "secondary" | "subtle" | "danger" | "default";

// What a control sits on: on a `raised` card a `raised` fill would vanish.
export type ControlSurface = "base" | "raised";

export type ButtonColors = {
  // `undefined` = transparent.
  fill: string | undefined;
  // Fill while pressed (enabled only).
  pressedFill: string | undefined;
  label: string;
};

export function controlFill(theme: Theme, surface: ControlSurface = "base"): string {
  return surface === "raised" ? theme.high : theme.raised;
}

/** The one disabled look every control shares: solid fill and `dim` foreground, no border. */
export function controlDisabled(
  theme: Theme,
  surface: ControlSurface = "base",
): {
  fill: string;
  fg: string;
} {
  return { fill: controlFill(theme, surface), fg: theme.dim };
}

/** A disabled PRIMARY keeps its identity (review6 #10): dimmed accent fill, light `text` label. */
export function disabledPrimary(theme: Theme): { fill: string; fg: string } {
  return { fill: theme.accentDisabled, fg: theme.text };
}

/** A checkbox box (review6 #9): the check glyph, not the hue, carries the state. */
export function checkboxColors(
  theme: Theme,
  checked: boolean,
): { fill: string; glyph: string } {
  if (checked) {
    return { fill: theme.accent, glyph: theme.onAccent };
  }
  return { fill: theme.muted, glyph: theme.onAccent };
}

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
