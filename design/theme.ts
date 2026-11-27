// The one colour source for every Pollis UI: desktop Modern, desktop Terminal and mobile.
// Rules for using it live in docs/design/README.md. Pure and import-free, so the desktop
// renderer, the mobile app and both platforms' `node --test` suites all load this file as is.
//
// Everything is derived from exactly two inputs, an accent and a background. Modern is the
// base recipe and mobile uses it unchanged; Terminal is a variant that recolours the same
// token names (text in the accent, accent-tinted lines, flat HSL surface steps).

export const DEFAULT_ACCENT_HEX = "#fabf5a";
export const DEFAULT_BACKGROUND_HEX = "#0a0907";

/** Accent presets, the same list on every platform. `n` is the stable lookup name; labels are translated. */
export const ACCENT_PRESETS = [
  { n: "Amber", c: DEFAULT_ACCENT_HEX },
  { n: "Citron", c: "#c9d65a" },
  { n: "Mint", c: "#8ad6a7" },
  { n: "Glass", c: "#7ec5d6" },
  { n: "Lilac", c: "#bda3e0" },
  { n: "Rust", c: "#d68f5a" },
] as const;

/** Background presets (desktop offers them; every platform must stay legible on them). */
export const BACKGROUND_PRESETS = [
  { n: "Warm", c: DEFAULT_BACKGROUND_HEX },
  { n: "Neutral", c: "#0b0b0b" },
  { n: "Cool", c: "#090b0f" },
  { n: "Green", c: "#080b09" },
  { n: "Purple", c: "#0b090e" },
] as const;

/** The one destructive red, per ground: the bright one on dark backgrounds, the deep one on light. */
export const DANGER_ON_DARK_HEX = "#ff6b6b";
export const DANGER_ON_LIGHT_HEX = "#b02020";
/** Degraded state and changed keys. */
export const WARNING_HEX = "#f0c674";
/** "In a voice room" only. */
export const SUCCESS_HEX = "#69d39e";

export type ThemeVariant = "modern" | "terminal";

/** The semantic token set. Every platform names these the same way (docs/design/tokens.md). */
export type ThemeTokens = {
  bg: string;
  surface: string;
  surfaceRaised: string;
  surfaceHigh: string;
  text: string;
  textDim: string;
  textMuted: string;
  /** Hairline separators. Decorative only: never the only edge of a control. */
  border: string;
  borderSoft: string;
  /** The edge of a control (inputs, outlined buttons): at least 3:1 against `surfaceRaised`. */
  borderStrong: string;
  accent: string;
  accentBright: string;
  accentDim: string;
  accentMuted: string;
  /** Accent tints, opaque over `bg`: 10%, 16%, 22%, 55%. */
  accentFaint: string;
  accentSoft: string;
  accentMid: string;
  accentLine: string;
  /** A disabled primary button's fill. */
  accentDisabled: string;
  /** Text and icons on an accent fill. Never white. */
  onAccent: string;
  danger: string;
  /** Text and icons on a danger fill. */
  onDanger: string;
  warning: string;
  success: string;
  hover: string;
  /** The selected row. */
  active: string;
  focusRing: string;
  backdrop: string;
  /** Whether `bg` is a light ground (the derivation flips toward black). */
  isLight: boolean;
};

export type Rgb = [number, number, number];

/** "#rrggbb" or an "r, g, b" triplet. */
export function parseColor(input: string): Rgb {
  const s = input.trim();
  if (s.startsWith("#")) {
    const h = s.slice(1);
    return [parseInt(h.slice(0, 2), 16), parseInt(h.slice(2, 4), 16), parseInt(h.slice(4, 6), 16)];
  }
  const parts = s.split(",").map((n) => parseInt(n.trim(), 10));
  return [parts[0], parts[1], parts[2]];
}

export function toHex(c: Rgb): string {
  const to = (n: number) => Math.max(0, Math.min(255, Math.round(n))).toString(16).padStart(2, "0");
  return `#${to(c[0])}${to(c[1])}${to(c[2])}`;
}

function toRgba(c: Rgb, alpha: number): string {
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

function luminanceOf(c: Rgb): number {
  const lin = c.map((v) => {
    const s = Math.round(v) / 255;
    return s <= 0.03928 ? s / 12.92 : Math.pow((s + 0.055) / 1.055, 2.4);
  });
  return 0.2126 * lin[0] + 0.7152 * lin[1] + 0.0722 * lin[2];
}

/** WCAG contrast ratio between two opaque colours ("#rrggbb" or "r, g, b"). */
export function contrast(a: string, b: string): number {
  const la = luminanceOf(parseColor(a));
  const lb = luminanceOf(parseColor(b));
  return (Math.max(la, lb) + 0.05) / (Math.min(la, lb) + 0.05);
}

/** An `rgba()` token composited over an opaque ground, as the eye sees it. Opaque input passes through. */
export function flatten(token: string, over: string): string {
  const m = /^rgba\(([^)]+)\)$/.exec(token.trim());
  if (!m) {
    return token;
  }
  const [r, g, b, a] = m[1].split(",").map((n) => parseFloat(n.trim()));
  return toHex(mix([r, g, b], parseColor(over), a));
}

/** [h 0–360, s 0–100, l 0–100]. */
export function rgbToHsl(c: Rgb): [number, number, number] {
  const r = c[0] / 255;
  const g = c[1] / 255;
  const b = c[2] / 255;
  const max = Math.max(r, g, b);
  const min = Math.min(r, g, b);
  const l = (max + min) / 2;
  if (max === min) {
    return [0, 0, l * 100];
  }
  const d = max - min;
  const s = l > 0.5 ? d / (2 - max - min) : d / (max + min);
  let h: number;
  if (max === r) {
    h = ((g - b) / d + (g < b ? 6 : 0)) / 6;
  } else if (max === g) {
    h = ((b - r) / d + 2) / 6;
  } else {
    h = ((r - g) / d + 4) / 6;
  }
  return [h * 360, s * 100, l * 100];
}

export function hslToRgb(h: number, s: number, l: number): Rgb {
  const sl = Math.max(0, Math.min(100, s)) / 100;
  const ll = Math.max(0, Math.min(100, l)) / 100;
  const a = sl * Math.min(ll, 1 - ll);
  const f = (n: number) => {
    const k = (n + h / 30) % 12;
    return 255 * (ll - a * Math.max(Math.min(k - 3, 9 - k, 1), -1));
  };
  return [f(0), f(8), f(4)];
}

/** A light ground flips every "toward white" step to "toward black". */
export function isLightGround(bgHex: string): boolean {
  return luminanceOf(parseColor(bgHex)) > 0.18;
}

// Modern's recipe: each tier is the accent mixed into the ground pushed toward white
// (toward black on a light ground). [accent weight, ground pushed by]
const RECIPE = {
  text: [0.22, 0.95],
  textDim: [0.3, 0.69],
  textMuted: [0.34, 0.55],
  surface: [0.05, 0.025],
  surfaceRaised: [0.07, 0.048],
  surfaceHigh: [0.1, 0.075],
  borderStrong: [0.5, 0.3],
} as const;

/** Ink for a fill: the ground when it reads at 4.5:1, else whichever of black and white reads better. */
function inkOn(fill: Rgb, ground: Rgb): string {
  const fillHex = toHex(fill);
  const groundHex = toHex(ground);
  if (contrast(groundHex, fillHex) >= 4.5) {
    return groundHex;
  }
  return contrast("#000000", fillHex) >= contrast("#ffffff", fillHex) ? "#0a0a0a" : "#ffffff";
}

/** The shared tokens both variants compute the same way. */
function common(a: Rgb, bg: Rgb, light: boolean) {
  const [ah, as, al] = rgbToHsl(a);
  const danger = parseColor(light ? DANGER_ON_LIGHT_HEX : DANGER_ON_DARK_HEX);
  return {
    accent: toHex(a),
    accentBright: toHex(hslToRgb(ah, as, al + 13)),
    accentDim: toHex(hslToRgb(ah, 45, 45)),
    accentMuted: toHex(hslToRgb(ah, 35, 25)),
    accentFaint: toHex(mix(a, bg, 0.1)),
    accentSoft: toHex(mix(a, bg, 0.16)),
    accentMid: toHex(mix(a, bg, 0.22)),
    accentLine: toHex(mix(a, bg, 0.55)),
    accentDisabled: toHex(mix(a, bg, 0.4)),
    onAccent: inkOn(a, bg),
    danger: toHex(danger),
    onDanger: inkOn(danger, bg),
    warning: WARNING_HEX,
    success: SUCCESS_HEX,
    backdrop: toRgba(mix(BLACK, bg, 0.6), 0.62),
    isLight: light,
  };
}

/** Modern: the base look, and mobile's look. */
function deriveModern(a: Rgb, bg: Rgb, light: boolean): ThemeTokens {
  const toward = light ? BLACK : WHITE;
  const tier = (k: keyof typeof RECIPE) => toHex(mix(a, mix(toward, bg, RECIPE[k][1]), RECIPE[k][0]));
  return {
    ...common(a, bg, light),
    bg: toHex(bg),
    surface: tier("surface"),
    surfaceRaised: tier("surfaceRaised"),
    surfaceHigh: tier("surfaceHigh"),
    text: tier("text"),
    textDim: tier("textDim"),
    textMuted: tier("textMuted"),
    border: toRgba(a, 0.16),
    borderSoft: toRgba(a, 0.1),
    borderStrong: tier("borderStrong"),
    hover: toRgba(toward, 0.05),
    active: toRgba(a, 0.13),
    focusRing: toRgba(a, 0.22),
  };
}

/** The accent, darkened (light ground) or lightened (dark ground) until it reads at 4.5:1 on `ground`. */
function readableAccent(a: Rgb, ground: Rgb, light: boolean): string {
  const [h, s, l] = rgbToHsl(a);
  const groundHex = toHex(ground);
  for (let step = 0; step <= 100; step += 2) {
    const candidate = toHex(hslToRgb(h, s, light ? l - step : l + step));
    if (contrast(candidate, groundHex) >= 4.5) {
      return candidate;
    }
  }
  return light ? "#000000" : "#ffffff";
}

/** Terminal: text in the accent, accent-tinted lines, surfaces as flat lightness steps. */
function deriveTerminal(a: Rgb, bg: Rgb, light: boolean): ThemeTokens {
  const [ah] = rgbToHsl(a);
  const [bh, bs, bl] = rgbToHsl(bg);
  const step = light ? -3 : 3;
  return {
    ...common(a, bg, light),
    bg: toHex(bg),
    surface: toHex(hslToRgb(bh, bs, bl + step)),
    surfaceRaised: toHex(hslToRgb(bh, bs, bl + step * 2)),
    surfaceHigh: toHex(hslToRgb(bh, bs, bl + step * 3)),
    // The accent itself, nudged only when a ground would make it unreadable.
    text: readableAccent(a, hslToRgb(bh, bs, bl + step * 3), light),
    textDim: toHex(hslToRgb(ah, 28, light ? 30 : 70)),
    textMuted: toHex(hslToRgb(ah, 18, light ? 36 : 59)),
    border: toRgba(a, 0.22),
    borderSoft: toRgba(a, 0.12),
    borderStrong: toRgba(a, 0.66),
    hover: toRgba(a, 0.08),
    active: toRgba(a, 0.14),
    focusRing: toRgba(a, 0.22),
  };
}

/** Accepts "#rrggbb" or an "r, g, b" triplet for either base. */
export function deriveTokens(accent: string, background: string, variant: ThemeVariant = "modern"): ThemeTokens {
  const a = parseColor(accent);
  const bg = parseColor(background);
  const light = isLightGround(toHex(bg));
  return variant === "terminal" ? deriveTerminal(a, bg, light) : deriveModern(a, bg, light);
}

/** "#RGB"/"rrggbb"/"#rrggbb" in any case → "#rrggbb", or null. */
export function normalizeHex(value: unknown): string | null {
  if (typeof value !== "string") {
    return null;
  }
  const v = value.trim().toLowerCase();
  const hex = v.startsWith("#") ? v.slice(1) : v;
  if (/^[0-9a-f]{3}$/.test(hex)) {
    return `#${hex[0]}${hex[0]}${hex[1]}${hex[1]}${hex[2]}${hex[2]}`;
  }
  if (!/^[0-9a-f]{6}$/.test(hex)) {
    return null;
  }
  return `#${hex}`;
}

/**
 * The synced colour preferences. `accent_color` / `background_color` are the keys every platform
 * reads; mobile builds before the unification read only `accent_hex`, so a save writes it too
 * (`colorPreferencesPatch`) until the oldest mobile build still in the stores reads `accent_color`.
 */
export function accentFromPrefs(prefs: { accent_color?: unknown; accent_hex?: unknown } | null | undefined): string | null {
  if (!prefs) {
    return null;
  }
  return normalizeHex(prefs.accent_color) ?? normalizeHex(prefs.accent_hex);
}

export function backgroundFromPrefs(prefs: { background_color?: unknown } | null | undefined): string | null {
  if (!prefs) {
    return null;
  }
  return normalizeHex(prefs.background_color);
}

/**
 * Motion (#2125): the timings and the one curve every Pollis animation uses, desktop and mobile.
 * Rules (docs/design/README.md → Motion): nothing runs longer than `slowMs`, nothing overshoots
 * its resting value by more than `maxOvershoot`, no glow, and under the OS reduced-motion setting
 * every movement becomes an opacity fade or nothing. Desktop paints these as `--motion-*`
 * variables (`frontend/src/utils/motion.ts`); mobile reads them through `mobile/theme/motion.ts`.
 */
export const MOTION = {
  fastMs: 120,
  baseMs: 180,
  slowMs: 240,
  /** A closing sheet: its entrance played backwards, a little quicker. */
  exitMs: 140,
  /** The one ease-out curve, as cubic-bezier control points. */
  easeOut: [0.22, 1, 0.36, 1] as const,
  /** Its mirror, for an exit that plays an entrance backwards. */
  easeIn: [0.64, 0, 0.78, 0] as const,
  /** A new message rises this far (px) as it fades in. */
  messageRisePx: 10,
  /** A popover rises this far (px) from the control that opened it. */
  popoverRisePx: 4,
  /** A new reaction pill or unread badge grows from this scale… */
  popFromScale: 0.7,
  /** …peaks here, then settles at 1. */
  popPeakScale: 1.08,
  /** A tapped reaction pill swells to this and back. */
  bumpScale: 1.08,
  /** An emoji under the pointer in the desktop picker (a resting state, not an overshoot). */
  hoverScale: 1.15,
  /** The ceiling for any overshoot past a resting value of 1. */
  maxOvershoot: 0.08,
} as const;

/** `cubic-bezier(…)` for CSS. */
export function cssCurve(points: readonly [number, number, number, number]): string {
  return `cubic-bezier(${points.join(", ")})`;
}

/** What a colour save writes: the shared key plus the legacy mobile mirror. */
export function colorPreferencesPatch(colors: { accent?: string; background?: string }): {
  accent_color?: string;
  accent_hex?: string;
  background_color?: string;
} {
  const out: { accent_color?: string; accent_hex?: string; background_color?: string } = {};
  const accent = normalizeHex(colors.accent);
  if (accent) {
    out.accent_color = accent;
    out.accent_hex = accent;
  }
  const background = normalizeHex(colors.background);
  if (background) {
    out.background_color = background;
  }
  return out;
}
