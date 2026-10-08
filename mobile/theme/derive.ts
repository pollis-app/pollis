// Pure colour derivation for the mobile theme, free of react-native so
// `node --test` can load it (tests/theme-contrast.test.ts).
//
// The theme has exactly TWO base colours: the accent and the background.
// Every other token is derived from them by mixing the background toward
// white (lighter neutrals), mixing the accent into a neutral (tint), or
// applying the accent at an alpha (hairlines). Change either base and the
// whole ramp re-derives.

export type Rgb = [number, number, number];

/** Parses "#rrggbb" or an "r, g, b" triplet into channels. */
export function parseColor(input: string): Rgb {
  const s = input.trim();
  if (s.startsWith("#")) {
    const h = s.slice(1);
    return [
      parseInt(h.slice(0, 2), 16),
      parseInt(h.slice(2, 4), 16),
      parseInt(h.slice(4, 6), 16),
    ];
  }
  const parts = s.split(",").map((n) => parseInt(n.trim(), 10));
  return [parts[0], parts[1], parts[2]];
}

/** Channels → "#rrggbb". */
export function toHex(c: Rgb): string {
  const to = (n: number) =>
    Math.max(0, Math.min(255, Math.round(n))).toString(16).padStart(2, "0");
  return `#${to(c[0])}${to(c[1])}${to(c[2])}`;
}

/** Channels + alpha → "rgba(r, g, b, a)". */
export function toRgba(c: Rgb, alpha: number): string {
  return `rgba(${c.map((n) => Math.round(n)).join(", ")}, ${alpha})`;
}

/**
 * `weight` of `a` mixed into `b` — the same as CSS
 * `color-mix(in srgb, a weight%, b)`.
 */
export function mix(a: Rgb, b: Rgb, weight: number): Rgb {
  return [
    a[0] * weight + b[0] * (1 - weight),
    a[1] * weight + b[1] * (1 - weight),
    a[2] * weight + b[2] * (1 - weight),
  ];
}

const WHITE: Rgb = [255, 255, 255];
const BLACK: Rgb = [0, 0, 0];

/** The background moved `amount` (0–1) of the way toward white. */
export function lighten(bg: Rgb, amount: number): Rgb {
  return mix(WHITE, bg, amount);
}

/** The background moved `amount` (0–1) of the way toward black. */
export function darken(bg: Rgb, amount: number): Rgb {
  return mix(BLACK, bg, amount);
}

/** WCAG 2.x relative luminance. */
export function luminance(c: Rgb): number {
  const lin = c.map((v) => {
    const s = Math.round(v) / 255;
    return s <= 0.03928 ? s / 12.92 : Math.pow((s + 0.055) / 1.055, 2.4);
  });
  return 0.2126 * lin[0] + 0.7152 * lin[1] + 0.0722 * lin[2];
}

/** WCAG contrast ratio between two opaque colours ("#hex" or triplet). */
export function contrast(a: string, b: string): number {
  const la = luminance(parseColor(a));
  const lb = luminance(parseColor(b));
  const hi = Math.max(la, lb);
  const lo = Math.min(la, lb);
  return (hi + 0.05) / (lo + 0.05);
}

// The recipe. Each tier is `accent` mixed into a neutral that is itself the
// background lightened toward white, so a different background re-derives
// the neutrals and a different accent re-tints them. With the default
// background (#0a0907) the neutrals land on the mockups' #f3f1ec / #b9b3a9 /
// #958f84 / #100e0b / #161411 / #1d1a16 / #3b3833.
//   [accent weight, background lightened by]
const RECIPE = {
  text: [0.22, 0.95],
  dim: [0.3, 0.69],
  muted: [0.34, 0.55],
  panel: [0.05, 0.025],
  raised: [0.07, 0.048],
  high: [0.1, 0.075],
  edge: [0.5, 0.2],
} as const;

/** Every colour token, as an opaque "#rrggbb" or an "rgba(…)" string. */
export type Theme = {
  // The two bases.
  accent: string;
  bg: string;
  // Text ramp.
  text: string;
  dim: string;
  muted: string;
  // Surface ramp: bg < panel < raised < high.
  panel: string;
  raised: string;
  high: string;
  // Hairlines (translucent accent) and the opaque control border.
  hair: string;
  hairSoft: string;
  edge: string;
  // Accent-tinted fills, opaque over the background.
  accentFaint: string;
  accentSoft: string;
  accentMid: string;
  accentLine: string;
  // A disabled primary button's fill: the accent dimmed toward the
  // background (theme/button.ts), so it keeps a primary identity.
  accentDisabled: string;
  // Text/icons drawn ON an accent fill.
  onAccent: string;
  // Destructive actions. There is no third hue (two-colour rule): destructive
  // rows are told apart by label, icon and separation, so this is the accent.
  danger: string;
  // Scrim behind sheets — the background darkened, translucent.
  backdrop: string;
};

/**
 * Derives the whole theme from the two bases. Accepts "#rrggbb" or an
 * "r, g, b" triplet for either argument.
 */
export function deriveTheme(accentRgb: string, bgRgb: string): Theme {
  const a = parseColor(accentRgb);
  const bg = parseColor(bgRgb);
  const tier = (k: keyof typeof RECIPE) =>
    toHex(mix(a, lighten(bg, RECIPE[k][1]), RECIPE[k][0]));
  return {
    accent: toHex(a),
    bg: toHex(bg),
    text: tier("text"),
    dim: tier("dim"),
    muted: tier("muted"),
    panel: tier("panel"),
    raised: tier("raised"),
    high: tier("high"),
    hair: toRgba(a, 0.16),
    hairSoft: toRgba(a, 0.1),
    edge: tier("edge"),
    accentFaint: toHex(mix(a, bg, 0.1)),
    accentSoft: toHex(mix(a, bg, 0.16)),
    accentMid: toHex(mix(a, bg, 0.22)),
    accentLine: toHex(mix(a, bg, 0.55)),
    accentDisabled: toHex(mix(a, bg, 0.4)),
    onAccent: toHex(bg),
    danger: toHex(a),
    backdrop: toRgba(darken(bg, 0.6), 0.62),
  };
}
