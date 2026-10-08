// Button colours per variant and state, pure (no react-native) so
// tests/button-contrast.test.ts can check every pair against the derived
// theme for every accent preset.
//
// Disabled is drawn with SOLID derived tokens, never `opacity` on the whole
// button: on Android a translucent parent fades each child separately (no
// offscreen compositing), so the label blended into the faded fill and fell
// far below 4.5:1 (#3/#4). Disabled is also told apart from enabled by more
// than colour: the fill steps down to the plain `raised` surface and the
// border turns dashed.

import type { Theme } from "./derive";

export type ButtonVariant = "primary" | "secondary" | "subtle" | "danger" | "default";

export type ButtonColors = {
  // `undefined` = transparent (the label then sits on whatever is behind).
  fill: string | undefined;
  // Fill while pressed (enabled only; enabled buttons also dim slightly on
  // press — transient, so its effect on the label doesn't matter).
  pressedFill: string | undefined;
  border: string | undefined;
  borderStyle: "solid" | "dashed";
  label: string;
};

/** The colours a Button paints with, from the current theme. */
export function buttonColors(
  theme: Theme,
  variant: ButtonVariant,
  disabled: boolean,
): ButtonColors {
  const subtle = variant === "subtle";
  if (disabled) {
    return {
      fill: subtle ? undefined : theme.raised,
      pressedFill: subtle ? undefined : theme.raised,
      border: theme.edge,
      borderStyle: "dashed",
      label: theme.dim,
    };
  }
  if (variant === "primary") {
    return {
      fill: theme.accent,
      pressedFill: theme.accent,
      border: undefined,
      borderStyle: "solid",
      label: theme.onAccent,
    };
  }
  if (subtle) {
    return {
      fill: undefined,
      pressedFill: theme.raised,
      border: undefined,
      borderStyle: "solid",
      label: theme.text,
    };
  }
  return {
    fill: theme.raised,
    pressedFill: theme.raised,
    border: theme.edge,
    borderStyle: "solid",
    label: theme.text,
  };
}
