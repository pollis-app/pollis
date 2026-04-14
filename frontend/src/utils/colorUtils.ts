import { setBackgroundLightness } from "./usernameColor";

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

/** Apply a hex accent color to the document CSS variables */
export function applyAccentColor(hex: string): void {
  const [h, s, l] = hexToHsl(hex);
  document.documentElement.style.setProperty("--accent-h", String(h));
  document.documentElement.style.setProperty("--accent-s", `${s}%`);
  document.documentElement.style.setProperty("--accent-l", `${l}%`);
}

/** Apply a hex background color to the document CSS variables */
export function applyBackgroundColor(hex: string): void {
  const [h, s, l] = hexToHsl(hex);
  document.documentElement.style.setProperty("--bg-h", String(h));
  document.documentElement.style.setProperty("--bg-s", `${s}%`);
  document.documentElement.style.setProperty("--bg-l", `${l}%`);
  // Notify subscribers (e.g. username coloring) that contrast target shifted.
  setBackgroundLightness(l);
}

/** Apply a font size (in px) to the document CSS variable */
export function applyFontSize(px: number): void {
  document.documentElement.style.setProperty("--font-size-base", `${px}px`);
}

/** Read the current accent color CSS vars as a hex string */
export function readAccentHex(): string {
  const h = parseInt(getComputedStyle(document.documentElement).getPropertyValue("--accent-h").trim() || "150", 10);
  const s = parseFloat(getComputedStyle(document.documentElement).getPropertyValue("--accent-s").trim() || "62");
  return hslToHex(h, s, 62);
}

/** Read the current background color CSS vars as a hex string */
export function readBackgroundHex(): string {
  const h = parseInt(getComputedStyle(document.documentElement).getPropertyValue("--bg-h").trim() || "38", 10);
  const s = parseFloat(getComputedStyle(document.documentElement).getPropertyValue("--bg-s").trim() || "20");
  return hslToHex(h, s, 20);
}

/** Read the current --font-size-base as a number (px) */
export function readFontSizePx(): number {
  const v = getComputedStyle(document.documentElement).getPropertyValue("--font-size-base").trim();
  return v ? parseInt(v, 10) : 15;
}
