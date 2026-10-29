import type { TFunction } from "i18next";
import { ACCENT_PRESETS } from "../../theme/tokens";

export type AccentPreset = (typeof ACCENT_PRESETS)[number];

// One literal call per value so `i18n-check` sees every key.
export function accentPresetLabel(t: TFunction, n: AccentPreset["n"]): string {
  switch (n) {
    case "Amber":
      return t("mobile:self.preferences.swatch.amber");
    case "Citron":
      return t("mobile:self.preferences.swatch.citron");
    case "Mint":
      return t("mobile:self.preferences.swatch.mint");
    case "Glass":
      return t("mobile:self.preferences.swatch.glass");
    case "Lilac":
      return t("mobile:self.preferences.swatch.lilac");
    case "Rust":
      return t("mobile:self.preferences.swatch.rust");
  }
}

/** Null for a custom accent. */
export function accentPresetFor(hex: string): AccentPreset | null {
  const wanted = hex.toLowerCase();
  return ACCENT_PRESETS.find((p) => p.c.toLowerCase() === wanted) ?? null;
}

export function accentDisplayName(t: TFunction, hex: string): string {
  const preset = accentPresetFor(hex);
  return preset ? accentPresetLabel(t, preset.n) : t("mobile:self.hub.customAccent");
}
