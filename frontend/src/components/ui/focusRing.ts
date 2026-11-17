// The one focus ring (#1964): 2px, offset by the ground (`bg`) so it never draws a black band
// on a lighter ground, and only for keyboard focus. Every control uses it; a destructive one
// takes the danger ring so its focus never wears the accent (docs/design/README.md).

/** Keyboard focus on a control. */
export const FOCUS_RING =
  "focus:outline-none focus-visible:ring-2 focus-visible:ring-accent focus-visible:ring-offset-2 focus-visible:ring-offset-bg";

/** Keyboard focus on a destructive control. */
export const FOCUS_RING_DANGER =
  "focus:outline-none focus-visible:ring-2 focus-visible:ring-danger focus-visible:ring-offset-2 focus-visible:ring-offset-bg";

/** Focus anywhere inside a wrapper (a field drawn around a native input). */
export const FOCUS_WITHIN_RING =
  "focus-within:ring-2 focus-within:ring-accent focus-within:ring-offset-2 focus-within:ring-offset-bg";
