// The accent presets and the two default bases. Pure.

export const DEFAULT_ACCENT_HEX = "#fabf5a";
export const DEFAULT_BACKGROUND_HEX = "#0a0907";

// `n` is the stable lookup name; the label is translated.
export const ACCENT_PRESETS = [
  { n: "Amber", c: DEFAULT_ACCENT_HEX },
  { n: "Citron", c: "#c9d65a" },
  { n: "Mint", c: "#8ad6a7" },
  { n: "Glass", c: "#7ec5d6" },
  { n: "Lilac", c: "#bda3e0" },
  { n: "Rust", c: "#d68f5a" },
] as const;
