import { Platform } from "react-native";
import { deriveTheme, type Theme } from "./derive";
import { loadBootAccent, loadBootBackground } from "./accentStore";
import { deviceAccentBackend } from "./deviceAccent";

export { DEFAULT_ACCENT_HEX, DEFAULT_BACKGROUND_HEX, ACCENT_PRESETS } from "./accents";

// Exactly TWO base colours, accent and background; everything else is derived (theme/derive.ts).

// The colours this device last showed (#1447), read synchronously at load so a cold start never flashes the defaults.
const _bootAccentHex = loadBootAccent(deviceAccentBackend);
let _accentRgb = hexToRgbTriplet(_bootAccentHex);
let _bgRgb = hexToRgbTriplet(loadBootBackground(deviceAccentBackend));
let _theme: Theme = deriveTheme(_accentRgb, _bgRgb);

export function setAccentRgb(rgb: string) {
  _accentRgb = rgb;
  _theme = deriveTheme(_accentRgb, _bgRgb);
}

export function setBackgroundRgb(rgb: string) {
  _bgRgb = rgb;
  _theme = deriveTheme(_accentRgb, _bgRgb);
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

// Script-dependent metrics (#1726), set by i18n/index.ts on every language
// change and read through the getters in `type` below.
// - Nastaliq (Urdu, Shahmukhi Punjabi) hangs far above and below the line, so
//   the Latin-sized line heights clip it.
// - Tracking is Latin typography: Android mis-measures tracked Arabic-script
//   text (a one-word title truncated to "مستق…"), and in the Indic scripts it
//   pulls clusters apart. The desktop drops it the same way (index.css).
const NASTALIQ_LINE_HEIGHT = 2;
let _nastaliq = false;
let _untracked = false;

export function setScriptMetrics(metrics: { nastaliq: boolean; untracked: boolean }) {
  _nastaliq = metrics.nastaliq;
  _untracked = metrics.untracked;
}

/** A Latin-sized line height, raised to fit Nastaliq while that script is active. */
export function scriptLineHeight(base: number, fontSize: number): number {
  return _nastaliq ? Math.max(base, Math.round(fontSize * NASTALIQ_LINE_HEIGHT)) : base;
}

/** A Latin letter-spacing, or none while an Arabic-script or Indic language is active. */
function scriptLetterSpacing(base: number): number {
  return _untracked ? 0 : base;
}

export function hexToRgbTriplet(hex: string): string {
  const h = hex.replace("#", "");
  const r = parseInt(h.slice(0, 2), 16);
  const g = parseInt(h.slice(2, 4), 16);
  const b = parseInt(h.slice(4, 6), 16);
  return `${r}, ${g}, ${b}`;
}

function rgbTripletToHex(rgb: string): string {
  const [r, g, b] = rgb.split(",").map((n) => parseInt(n.trim(), 10));
  const to = (n: number) => n.toString(16).padStart(2, "0");
  return `#${to(r)}${to(g)}${to(b)}`;
}

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
  // Text and icons on a danger fill.
  get onDanger() {
    return _theme.onDanger;
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

// Corners for one row of a virtualised grouped list: the first row rounds its
// top, the last its bottom, so the rows read as one card (a FlatList cannot
// sit inside <Group>).
export function groupedRowCorners(index: number, last: number) {
  return {
    borderTopLeftRadius: index === 0 ? r.lg : 0,
    borderTopRightRadius: index === 0 ? r.lg : 0,
    borderBottomLeftRadius: index === last ? r.lg : 0,
    borderBottomRightRadius: index === last ? r.lg : 0,
  };
}

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

// The type scale by role (#1973), the same names as desktop's `text-*` utilities; each platform
// keeps its own sizes (docs/design/README.md → Type). A `fontSize` in a component reads one of these.
export const typeSize = {
  meta: 12,
  section: 13,
  secondary: 14,
  // Button labels, and the denser rows sized like them.
  button: 15,
  body: 16,
  rowTitle: 16,
  heading: 17,
  title: 20,
  display: 28,
};

// Colours, line heights and letter-spacing are getters so a flattened style tracks the live theme and script.
export const type = {
  display: {
    fontFamily: fonts.bold,
    fontSize: typeSize.display,
    get lineHeight() {
      return scriptLineHeight(34, typeSize.display);
    },
    get letterSpacing() {
      return scriptLetterSpacing(-0.56);
    },
    get color() {
      return _theme.text;
    },
  },
  title: {
    fontFamily: fonts.bold,
    fontSize: typeSize.title,
    get lineHeight() {
      return scriptLineHeight(26, typeSize.title);
    },
    get letterSpacing() {
      return scriptLetterSpacing(-0.2);
    },
    get color() {
      return _theme.text;
    },
  },
  heading: {
    fontFamily: fonts.bold,
    fontSize: typeSize.heading,
    get lineHeight() {
      return scriptLineHeight(22, typeSize.heading);
    },
    get color() {
      return _theme.text;
    },
  },
  body: {
    fontFamily: fonts.regular,
    fontSize: typeSize.body,
    get lineHeight() {
      return scriptLineHeight(23, typeSize.body);
    },
    get color() {
      return _theme.text;
    },
  },
  secondary: {
    fontFamily: fonts.regular,
    fontSize: typeSize.secondary,
    get lineHeight() {
      return scriptLineHeight(19, typeSize.secondary);
    },
    get color() {
      return _theme.dim;
    },
  },
  section: {
    fontFamily: fonts.semibold,
    fontSize: typeSize.section,
    get lineHeight() {
      return scriptLineHeight(18, typeSize.section);
    },
    get color() {
      return _theme.dim;
    },
  },
  meta: {
    fontFamily: fonts.regular,
    fontSize: typeSize.meta,
    get lineHeight() {
      return scriptLineHeight(16, typeSize.meta);
    },
    get color() {
      return _theme.muted;
    },
  },
  // A list row's name: no line height of its own, so a row keeps the face's natural one.
  rowTitle: {
    fontFamily: fonts.semibold,
    fontSize: typeSize.rowTitle,
    get color() {
      return _theme.text;
    },
  },
  // An inline button's label; a full-width button's is `rowTitle`.
  button: {
    fontFamily: fonts.semibold,
    fontSize: typeSize.button,
  },
  tab: {
    fontFamily: fonts.medium,
    fontSize: typeSize.meta,
  },
  mono: { fontFamily: fonts.mono400, fontSize: 13 },

  /* ── Deprecated aliases — migrate to the names above ── */
  h1: {
    fontFamily: fonts.bold,
    fontSize: 20,
    get letterSpacing() {
      return scriptLetterSpacing(-0.2);
    },
  },
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
