import { Platform } from "react-native";

// Pollis Mobile — design tokens
// Dark, monochrome-amber. One bg color + one accent; everything else is a
// translucent tier of the accent. The accent is runtime-configurable (mirrors
// the desktop app's accent picker) — `semantic` / `palette.accent` are getters
// so changing the accent re-derives every tier.

// Brand default — same amber as the desktop app + website (#fabf5a).
const DEFAULT_ACCENT = "250, 191, 90";

let _accentRgb = DEFAULT_ACCENT;

export function setAccentRgb(rgb: string) {
  _accentRgb = rgb;
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

export const DEFAULT_ACCENT_HEX = rgbTripletToHex(DEFAULT_ACCENT);

// Translucent accent tier. Reads the *current* accent each call.
export const t = (alpha: number) => `rgba(${_accentRgb}, ${alpha})`;

export const palette = {
  bg: "#0a0907", // just-above-black
  bg2: "#100d09", // raised surface (status bar, etc.)
  bg3: "#1a1610", // hover / pressed
  danger: "#c46a2e",
  get accent() {
    return `rgb(${_accentRgb})`;
  },
};

// Getter object — every read re-derives from the live accent.
export const semantic = {
  get ink() {
    return t(1.0);
  },
  get ink2() {
    return t(0.7);
  },
  get mute() {
    return t(0.5);
  },
  get mute2() {
    return t(0.36);
  },
  get hair() {
    return t(0.12);
  },
  get hairSoft() {
    return t(0.08);
  },
  get hairStrong() {
    return t(0.24);
  },
  get accent() {
    return t(1.0);
  },
  get accentSoft() {
    return t(0.16);
  },
  get fieldBg() {
    return t(0.04);
  },
  get cardBg() {
    return t(0.06);
  },
  danger: palette.danger,
};

export const r = { sm: 3, lg: 4 };

// Irregular-but-consistent scale — do not normalize to an 8px grid.
export const space = { xs: 6, sm: 8, md: 10, lg: 12, xl: 14, xxl: 18, xxxl: 22 };

// System monospace — no bundled font. iOS has no generic "monospace" alias,
// so name the platform face explicitly.
const SYSTEM_MONO = Platform.select({
  ios: "Menlo",
  android: "monospace",
  default: "monospace",
});

export const fonts = {
  sora400: "Sora_400Regular",
  sora500: "Sora_500Medium",
  sora600: "Sora_600SemiBold",
  sora700: "Sora_700Bold",
  mono400: SYSTEM_MONO,
  mono500: SYSTEM_MONO,
};

// React Native has no em letter-spacing — values pre-converted (size * em).
// `color` is a getter so flattening the style each render tracks the live
// accent instead of baking in the default amber.
export const type = {
  h1: { fontFamily: fonts.sora600, fontSize: 22, letterSpacing: -0.22 },
  h2: { fontFamily: fonts.sora500, fontSize: 17, letterSpacing: -0.085 },
  body: { fontFamily: fonts.sora400, fontSize: 14 },
  rowN: { fontFamily: fonts.sora500, fontSize: 15 },
  rowSub: {
    fontFamily: fonts.sora400,
    fontSize: 12,
    get color() {
      return semantic.mute;
    },
  },
  label: {
    fontFamily: fonts.sora500,
    fontSize: 10,
    letterSpacing: 2.2,
    textTransform: "uppercase" as const,
    get color() {
      return semantic.mute;
    },
  },
  crumb: {
    fontFamily: fonts.sora400,
    fontSize: 10,
    letterSpacing: 1.8,
    textTransform: "uppercase" as const,
    get color() {
      return semantic.mute;
    },
  },
  mono: { fontFamily: fonts.mono400, fontSize: 13 },
};

export const layout = {
  statusBar: 38,
  tabBar: 70,
  ctx: 52,
  composer: 58,
  touchMin: 38,
};
