import { setBackgroundLightness } from "./usernameColor";
import {
  DEFAULT_ACCENT_HEX,
  DEFAULT_BACKGROUND_HEX,
  deriveTokens,
  normalizeHex,
  type ThemeTokens,
} from "../../../design/theme";
import { DEFAULT_SKIN, setSkinAttribute, type Skin } from "./skin";

/** Convert a hex color string to [h (0-360), s (0-100), l (0-100)] */
export function hexToHsl(hex: string): [number, number, number] {
  const r = parseInt(hex.slice(1, 3), 16) / 255;
  const g = parseInt(hex.slice(3, 5), 16) / 255;
  const b = parseInt(hex.slice(5, 7), 16) / 255;
  const max = Math.max(r, g, b);
  const min = Math.min(r, g, b);
  const l = (max + min) / 2;
  if (max === min) {
    return [0, 0, Math.round(l * 100)];
  }
  const d = max - min;
  const s = l > 0.5 ? d / (2 - max - min) : d / (max + min);
  let h = 0;
  if (max === r) { h = ((g - b) / d + (g < b ? 6 : 0)) / 6; }
  else if (max === g) { h = ((b - r) / d + 2) / 6; }
  else { h = ((r - g) / d + 4) / 6; }
  return [Math.round(h * 360), Math.round(s * 100), Math.round(l * 100)];
}

/** Convert h (0-360), s (0-100), l (0-100) to a hex string */
export function hslToHex(h: number, s: number, l: number): string {
  const sl = s / 100;
  const ll = l / 100;
  const a = sl * Math.min(ll, 1 - ll);
  const f = (n: number) => {
    const k = (n + h / 30) % 12;
    const color = ll - a * Math.max(Math.min(k - 3, 9 - k, 1), -1);
    return Math.round(255 * color).toString(16).padStart(2, "0");
  };
  return `#${f(0)}${f(8)}${f(4)}`;
}

/** The colours and skin the page is painted with right now. */
const current: { accent: string; background: string; skin: Skin } = {
  accent: DEFAULT_ACCENT_HEX,
  background: DEFAULT_BACKGROUND_HEX,
  skin: DEFAULT_SKIN,
};

/** CSS variable → shared token (design/theme.ts). `--c-border-active` is the historical name of `borderStrong`. */
const CSS_TOKENS: ReadonlyArray<[string, keyof ThemeTokens]> = [
  ["--c-bg", "bg"],
  ["--c-surface", "surface"],
  ["--c-surface-raised", "surfaceRaised"],
  ["--c-surface-high", "surfaceHigh"],
  ["--c-text", "text"],
  ["--c-text-dim", "textDim"],
  ["--c-text-muted", "textMuted"],
  ["--c-border", "border"],
  ["--c-border-soft", "borderSoft"],
  ["--c-border-active", "borderStrong"],
  ["--c-accent", "accent"],
  ["--c-accent-bright", "accentBright"],
  ["--c-accent-dim", "accentDim"],
  ["--c-accent-muted", "accentMuted"],
  ["--c-accent-faint", "accentFaint"],
  ["--c-accent-soft", "accentSoft"],
  ["--c-accent-mid", "accentMid"],
  ["--c-on-accent", "onAccent"],
  ["--c-danger", "danger"],
  ["--c-on-danger", "onDanger"],
  ["--c-warning", "warning"],
  ["--c-voice-connected", "success"],
  ["--c-hover", "hover"],
  ["--c-active", "active"],
  ["--c-focus-ring", "focusRing"],
];

/** Recompute every colour token from the current accent, background and skin, and write them on <html>. */
function paint(): void {
  const tokens = deriveTokens(current.accent, current.background, current.skin);
  const root = document.documentElement;
  for (const [name, key] of CSS_TOKENS) {
    root.style.setProperty(name, String(tokens[key]));
  }
  root.dataset.ground = tokens.isLight ? "light" : "dark";
  setBackgroundLightness(hexToHsl(current.background)[2]);
}

/** Paint the defaults (Modern, default accent and background) before the first render. */
export function initTheme(): void {
  setSkinAttribute(current.skin);
  paint();
}

/** The accent the page is painted with, as "#rrggbb". */
export function currentAccentHex(): string {
  return current.accent;
}

/** Apply a hex accent color; every token re-derives from it. */
export function applyAccentColor(hex: string): void {
  const normalized = normalizeHex(hex);
  if (!normalized) {
    return;
  }
  current.accent = normalized;
  paint();
}

/** Apply a hex background color; every token re-derives from it. */
export function applyBackgroundColor(hex: string): void {
  const normalized = normalizeHex(hex);
  if (!normalized) {
    return;
  }
  current.background = normalized;
  paint();
}

/** Apply the UI skin: the `data-skin` attribute for layout CSS, and the skin's colour branch. */
export function applySkin(skin: Skin): void {
  current.skin = skin;
  setSkinAttribute(skin);
  paint();
}

/** Apply a font size (in px) to the document CSS variable */
export function applyFontSize(px: number): void {
  document.documentElement.style.setProperty("--font-size-base", `${px}px`);
}

export { DEFAULT_SKIN, normalizeSkin, skinToWire, type Skin } from "./skin";

/** Device-local font size, keyed by user id. */
const FONT_SIZE_KEY_PREFIX = "pollis-font-size:";

function fontSizeKey(userId: string | null | undefined): string {
  return `${FONT_SIZE_KEY_PREFIX}${userId ?? "anon"}`;
}

/** Returns the device-local font size (px) for this user, or null if unset. */
export function loadDeviceFontSize(userId: string | null | undefined): number | null {
  try {
    const raw = localStorage.getItem(fontSizeKey(userId));
    if (!raw) {
      return null;
    }
    const px = parseInt(raw, 10);
    if (!Number.isFinite(px) || px < 10 || px > 28) {
      return null;
    }
    return px;
  } catch {
    return null;
  }
}

/** Persists the device-local font size (px) for this user. */
export function saveDeviceFontSize(userId: string | null | undefined, px: number): void {
  try {
    localStorage.setItem(fontSizeKey(userId), String(px));
  } catch {
  }
}
