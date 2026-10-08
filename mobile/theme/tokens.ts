import { Platform } from "react-native";
import { compositeOver } from "./composite";
import { deriveTheme, type Theme } from "./derive";
import { DEFAULT_ACCENT_HEX, DEFAULT_BACKGROUND_HEX } from "./accents";

export { DEFAULT_ACCENT_HEX, DEFAULT_BACKGROUND_HEX, ACCENT_PRESETS } from "./accents";

// Pollis Mobile — design tokens.
//
// Exactly TWO base colours: the accent and the background. Every other colour
// is derived from them in `theme/derive.ts` (pure, unit-tested by
// tests/theme-contrast.test.ts). Both bases are runtime-configurable; the
// `palette` / `semantic` / `type` colours are getters over the current derived
// theme, so changing a base re-derives everything on the next render.

let _accentRgb = hexToRgbTriplet(DEFAULT_ACCENT_HEX);
let _bgRgb = hexToRgbTriplet(DEFAULT_BACKGROUND_HEX);
let _theme: Theme = deriveTheme(_accentRgb, _bgRgb);

/** Sets the accent from an "r, g, b" triplet and re-derives the theme. */
export function setAccentRgb(rgb: string) {
  _accentRgb = rgb;
  _theme = deriveTheme(_accentRgb, _bgRgb);
}

/** Sets the background from an "r, g, b" triplet and re-derives the theme. */
export function setBackgroundRgb(rgb: string) {
  _bgRgb = rgb;
  _theme = deriveTheme(_accentRgb, _bgRgb);
}

/** Sets the accent from "#rrggbb". */
export function setAccentHex(hex: string) {
  setAccentRgb(hexToRgbTriplet(hex));
}

/** Sets the background from "#rrggbb". */
export function setBackgroundHex(hex: string) {
  setBackgroundRgb(hexToRgbTriplet(hex));
}

/** The current derived theme (every colour token as a string). */
export function currentTheme(): Theme {
  return _theme;
}

/** "#rrggbb" → "r, g, b". */
export function hexToRgbTriplet(hex: string): string {
  const h = hex.replace("#", "");
  const r = parseInt(h.slice(0, 2), 16);
  const g = parseInt(h.slice(2, 4), 16);
  const b = parseInt(h.slice(4, 6), 16);
  return `${r}, ${g}, ${b}`;
}

/** "r, g, b" → "#rrggbb". */
export function rgbTripletToHex(rgb: string): string {
  const [r, g, b] = rgb.split(",").map((n) => parseInt(n.trim(), 10));
  const to = (n: number) => n.toString(16).padStart(2, "0");
  return `#${to(r)}${to(g)}${to(b)}`;
}

// Translucent accent tier. Reads the *current* accent each call. Prefer the
// opaque semantic tokens; this stays for the few call sites that tint.
export const t = (alpha: number) => `rgba(${_accentRgb}, ${alpha})`;

// The same tier pre-composited over the background: identical colour where it
// sits on the bare background, but OPAQUE (#1193).
export const tOpaque = (alpha: number) => compositeOver(_accentRgb, alpha, _bgRgb);

// The two bases (plus deprecated aliases kept so old call sites compile).
export const palette = {
  get bg() {
    return _theme.bg;
  },
  get accent() {
    return _theme.accent;
  },
  // Deprecated: use semantic.panel.
  get bg2() {
    return _theme.panel;
  },
  // Deprecated: use semantic.raised.
  get bg3() {
    return _theme.raised;
  },
  // Deprecated: no third hue exists; this is the accent (see derive.ts).
  get danger() {
    return _theme.danger;
  },
};

// Getter object — every read resolves against the live derived theme.
export const semantic = {
  /* ── Current names ── */
  // Body text, names, titles.
  get text() {
    return _theme.text;
  },
  // Secondary text: previews, read channels, section titles, inactive tabs.
  get dim() {
    return _theme.dim;
  },
  // Tertiary text: timestamps, hints, placeholders, chevrons.
  get muted() {
    return _theme.muted;
  },
  // The background (screens, chat).
  get bg() {
    return _theme.bg;
  },
  // Surface tier 1: channel list sheet, tab bar, profile card.
  get panel() {
    return _theme.panel;
  },
  // Surface tier 2: inputs, grouped cards, sheets, chips.
  get raised() {
    return _theme.raised;
  },
  // Surface tier 3: avatars, groups inside a sheet, icon buttons on raised.
  get high() {
    return _theme.high;
  },
  // Hairline separators (translucent accent).
  get hair() {
    return _theme.hair;
  },
  get hairSoft() {
    return _theme.hairSoft;
  },
  // Opaque control border (fields, outline buttons) — ≥3:1 vs raised.
  get edge() {
    return _theme.edge;
  },
  // The accent itself: active, mentions, send, links.
  get accent() {
    return _theme.accent;
  },
  // Accent tints, opaque over bg: 10% (highlighted message row), 16%
  // (selected pill / row), 22% (mention chip, own avatar), 55% (selected pill
  // border).
  get accentFaint() {
    return _theme.accentFaint;
  },
  get accentSoft() {
    return _theme.accentSoft;
  },
  get accentMid() {
    return _theme.accentMid;
  },
  get accentLine() {
    return _theme.accentLine;
  },
  // Text and icons drawn on an accent fill.
  get onAccent() {
    return _theme.onAccent;
  },
  // Destructive actions — the accent (no third hue; see derive.ts).
  get danger() {
    return _theme.danger;
  },
  // Scrim behind sheets.
  get backdrop() {
    return _theme.backdrop;
  },
  // Sheet surface. Opaque (#1193): a translucent sheet lets the header and
  // composer show through its buttons.
  get sheetBg() {
    return _theme.raised;
  },

  /* ── Deprecated aliases (old ramp names) — migrate to the names above ── */
  get ink() {
    return _theme.text;
  },
  get ink2() {
    return _theme.dim;
  },
  get mute() {
    return _theme.muted;
  },
  get mute2() {
    return _theme.muted;
  },
  get hairStrong() {
    return _theme.edge;
  },
  get fieldBg() {
    return _theme.raised;
  },
  get cardBg() {
    return _theme.raised;
  },
};

// Corner radii (mockups): chips/pills are fully rounded (height / 2).
export const r = {
  xs: 4,
  sm: 10,
  md: 12,
  lg: 14,
  xl: 16,
  sheet: 20,
  pill: 999,
};

// Spacing scale.
export const space = { xs: 6, sm: 8, md: 10, lg: 12, xl: 14, xxl: 16, xxxl: 20 };

// System monospace — no bundled font. iOS has no generic "monospace" alias,
// so name the platform face explicitly.
const SYSTEM_MONO = Platform.select({
  ios: "Menlo",
  android: "monospace",
  default: "monospace",
});

// Geist, one family name per weight. Never combine these with `fontWeight`:
// Android then synthesises a weight instead of using the loaded face.
export const fonts = {
  regular: "Geist_400Regular",
  medium: "Geist_500Medium",
  semibold: "Geist_600SemiBold",
  bold: "Geist_700Bold",
  mono400: SYSTEM_MONO,
  mono500: SYSTEM_MONO,
  // Deprecated aliases from the Sora era — they resolve to Geist.
  sora400: "Geist_400Regular",
  sora500: "Geist_500Medium",
  sora600: "Geist_600SemiBold",
  sora700: "Geist_700Bold",
};

// Type scale (Tokens.dc.html). Sentence case throughout, no tracked capitals,
// nothing below 12. Colours are getters so a flattened style tracks the live
// theme.
export const type = {
  display: {
    fontFamily: fonts.bold,
    fontSize: 28,
    lineHeight: 34,
    letterSpacing: -0.56,
    get color() {
      return _theme.text;
    },
  },
  title: {
    fontFamily: fonts.bold,
    fontSize: 20,
    lineHeight: 26,
    letterSpacing: -0.2,
    get color() {
      return _theme.text;
    },
  },
  heading: {
    fontFamily: fonts.bold,
    fontSize: 17,
    lineHeight: 22,
    get color() {
      return _theme.text;
    },
  },
  body: {
    fontFamily: fonts.regular,
    fontSize: 16,
    lineHeight: 23,
    get color() {
      return _theme.text;
    },
  },
  secondary: {
    fontFamily: fonts.regular,
    fontSize: 14,
    lineHeight: 19,
    get color() {
      return _theme.dim;
    },
  },
  section: {
    fontFamily: fonts.semibold,
    fontSize: 13,
    lineHeight: 18,
    get color() {
      return _theme.dim;
    },
  },
  meta: {
    fontFamily: fonts.regular,
    fontSize: 12,
    lineHeight: 16,
    get color() {
      return _theme.muted;
    },
  },
  tab: {
    fontFamily: fonts.medium,
    fontSize: 12,
  },
  mono: { fontFamily: fonts.mono400, fontSize: 13 },

  /* ── Deprecated aliases — migrate to the names above ── */
  h1: { fontFamily: fonts.bold, fontSize: 20, letterSpacing: -0.2 },
  h2: { fontFamily: fonts.bold, fontSize: 17 },
  rowN: { fontFamily: fonts.medium, fontSize: 16 },
  rowSub: {
    fontFamily: fonts.regular,
    fontSize: 14,
    get color() {
      return _theme.dim;
    },
  },
  label: {
    fontFamily: fonts.semibold,
    fontSize: 13,
    get color() {
      return _theme.dim;
    },
  },
  crumb: {
    fontFamily: fonts.regular,
    fontSize: 12,
    get color() {
      return _theme.muted;
    },
  },
};

export const layout = {
  // Minimum touch target (Apple HIG / WCAG 2.5.5).
  touchMin: 44,
  // Header bar content height (below the status bar inset).
  header: 52,
  // Tab bar content height (above the home-indicator inset): a 10pt gap
  // under the hairline (the active-tab bar sits in it), the 22pt icon, the
  // 12pt label and a little air below.
  tabBar: 56,
  composer: 58,
  // Deprecated: the old bottom context strip.
  ctx: 52,
  statusBar: 38,
  // Max width for a single-column screen on regular (iPad) width.
  readableMaxWidth: 560,
  // Two-pane left (list) column width.
  listPaneWidth: 340,
};
