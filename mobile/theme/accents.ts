// The accent presets offered by Self → Preferences → Accent, and the two
// default bases. Pure (no react-native) so tests/theme-contrast.test.ts can
// check every preset against the derived theme.

// Brand amber — same as the desktop app + website.
export const DEFAULT_ACCENT_HEX = "#fabf5a";
// Just-above-black.
export const DEFAULT_BACKGROUND_HEX = "#0a0907";

// `n` is the stable wire/lookup name; the rendered label is translated.
export const ACCENT_PRESETS = [
  { n: "Amber", c: DEFAULT_ACCENT_HEX },
  { n: "Citron", c: "#c9d65a" },
  { n: "Mint", c: "#8ad6a7" },
  { n: "Glass", c: "#7ec5d6" },
  { n: "Lilac", c: "#bda3e0" },
  { n: "Rust", c: "#d68f5a" },
] as const;
