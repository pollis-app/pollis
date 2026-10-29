// Pure colour derivation from exactly TWO bases, accent and background.

export type Rgb = [number, number, number];

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

export function toHex(c: Rgb): string {
  const to = (n: number) =>
    Math.max(0, Math.min(255, Math.round(n))).toString(16).padStart(2, "0");
  return `#${to(c[0])}${to(c[1])}${to(c[2])}`;
}

export function toRgba(c: Rgb, alpha: number): string {
  return `rgba(${c.map((n) => Math.round(n)).join(", ")}, ${alpha})`;
}

/** Same as CSS `color-mix(in srgb, a weight%, b)`. */
export function mix(a: Rgb, b: Rgb, weight: number): Rgb {
  return [
    a[0] * weight + b[0] * (1 - weight),
    a[1] * weight + b[1] * (1 - weight),
    a[2] * weight + b[2] * (1 - weight),
  ];
}

const WHITE: Rgb = [255, 255, 255];
const BLACK: Rgb = [0, 0, 0];

export function lighten(bg: Rgb, amount: number): Rgb {
  return mix(WHITE, bg, amount);
}

export function darken(bg: Rgb, amount: number): Rgb {
  return mix(BLACK, bg, amount);
}

export function luminance(c: Rgb): number {
  const lin = c.map((v) => {
    const s = Math.round(v) / 255;
    return s <= 0.03928 ? s / 12.92 : Math.pow((s + 0.055) / 1.055, 2.4);
  });
  return 0.2126 * lin[0] + 0.7152 * lin[1] + 0.0722 * lin[2];
}

export function contrast(a: string, b: string): number {
  const la = luminance(parseColor(a));
  const lb = luminance(parseColor(b));
  const hi = Math.max(la, lb);
  const lo = Math.min(la, lb);
  return (hi + 0.05) / (lo + 0.05);
}

// Each tier is the accent mixed into the background lightened toward white.
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

export type Theme = {
  accent: string;
  bg: string;
  text: string;
  dim: string;
  muted: string;
  panel: string;
  raised: string;
  high: string;
  hair: string;
  hairSoft: string;
  edge: string;
  accentFaint: string;
  accentSoft: string;
  accentMid: string;
  accentLine: string;
  // A disabled primary button's fill (theme/button.ts).
  accentDisabled: string;
  onAccent: string;
  // No third hue (two-colour rule): this is the accent.
  danger: string;
  backdrop: string;
};

/** Accepts "#rrggbb" or an "r, g, b" triplet for either base. */
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
