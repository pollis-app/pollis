import { Platform } from "react-native";
import { compositeOver } from "./composite";
import { deriveTheme, type Theme } from "./derive";
import { DEFAULT_BACKGROUND_HEX } from "./accents";
import { loadBootAccent } from "./accentStore";
import { deviceAccentBackend } from "./deviceAccent";

export { DEFAULT_ACCENT_HEX, DEFAULT_BACKGROUND_HEX, ACCENT_PRESETS } from "./accents";

// Exactly TWO base colours, accent and background; everything else is derived (theme/derive.ts).

// The accent this device last showed (#1447), read synchronously at load so a cold start never flashes amber.
const _bootAccentHex = loadBootAccent(deviceAccentBackend);
let _accentRgb = hexToRgbTriplet(_bootAccentHex);
let _bgRgb = hexToRgbTriplet(DEFAULT_BACKGROUND_HEX);
let _theme: Theme = deriveTheme(_accentRgb, _bgRgb);

export function setAccentRgb(rgb: string) {
  _accentRgb = rgb;
  _theme = deriveTheme(_accentRgb, _bgRgb);
}

export function setBackgroundRgb(rgb: string) {
  _bgRgb = rgb;
  _theme = deriveTheme(_accentRgb, _bgRgb);
}

export function setAccentHex(hex: string) {
  setAccentRgb(hexToRgbTriplet(hex));
}

export function setBackgroundHex(hex: string) {
  setBackgroundRgb(hexToRgbTriplet(hex));
}

export function currentAccentHex(): string {
  return rgbTripletToHex(_accentRgb);
}

/** The accent the tokens were seeded with at load — what the first frame painted. */
export function bootAccentHex(): string {
  return _bootAccentHex;
}

export function currentBackgroundHex(): string {
  return rgbTripletToHex(_bgRgb);
}

export function currentTheme(): Theme {
  return _theme;
}

export function hexToRgbTriplet(hex: string): string {
  const h = hex.replace("#", "");
  const r = parseInt(h.slice(0, 2), 16);
  const g = parseInt(h.slice(2, 4), 16);
  const b = parseInt(h.slice(4, 6), 16);
  return `${r}, ${g}, ${b}`;
}

export function rgbTripletToHex(rgb: string): string {
  const [r, g, b] = rgb.split(",").map((n) => parseInt(n.trim(), 10));
  const to = (n: number) => n.toString(16).padStart(2, "0");
  return `#${to(r)}${to(g)}${to(b)}`;
}

// Translucent accent tier. Prefer the opaque semantic tokens.
export const t = (alpha: number) => `rgba(${_accentRgb}, ${alpha})`;

// The same tier pre-composited over the background: OPAQUE (#1193).
export const tOpaque = (alpha: number) => compositeOver(_accentRgb, alpha, _bgRgb);

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

export const semantic = {
  /* ── Current names ── */
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
  get accent() {
    return _theme.accent;
  },
  // Accent tints, opaque over bg: 10%, 16%, 22%, 55%.
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
  // A disabled primary button's fill (40% accent over the background).
  get accentDisabled() {
    return _theme.accentDisabled;
  },
  get onAccent() {
    return _theme.onAccent;
  },
  // Destructive actions — the accent (no third hue; see derive.ts).
  get danger() {
    return _theme.danger;
  },
  get backdrop() {
    return _theme.backdrop;
  },
  // Opaque (#1193): a translucent sheet lets the header show through.
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

export const r = {
  xs: 4,
  sm: 10,
  md: 12,
  lg: 14,
  xl: 16,
  sheet: 20,
  pill: 999,
};

export const space = { xs: 6, sm: 8, md: 10, lg: 12, xl: 14, xxl: 16, xxxl: 20 };

// iOS has no generic "monospace" alias.
const SYSTEM_MONO = Platform.select({
  ios: "Menlo",
  android: "monospace",
  default: "monospace",
});

// One family per weight. Never with `fontWeight`: Android would synthesise a weight.
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

// Colours are getters so a flattened style tracks the live theme.
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
  touchMin: 44,
  header: 52,
  tabBar: 56,
  composer: 58,
  // Past it the field scrolls inside.
  composerInputMax: 140,
  // Deprecated: the old bottom context strip.
  ctx: 52,
  statusBar: 38,
  screenMaxWidth: 640,
  // The auth steps' narrower column (`<Screen centered>`).
  authMaxWidth: 560,
  sheetMaxWidth: 520,
  listPaneWidth: 340,
};
