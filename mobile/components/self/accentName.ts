import type { TFunction } from "i18next";
import { ACCENT_PRESETS } from "../../theme/tokens";

export type AccentPreset = (typeof ACCENT_PRESETS)[number];

// The wire values in ACCENT_PRESETS stay English; only the rendered label is
// keyed, one literal call per value so `i18n-check` can see every key.
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

/** The preset matching `hex`, or null for a custom accent (e.g. synced from desktop). */
export function accentPresetFor(hex: string): AccentPreset | null {
  const wanted = hex.toLowerCase();
  return ACCENT_PRESETS.find((p) => p.c.toLowerCase() === wanted) ?? null;
}

/** Display name for the current accent: the preset's name, else "Custom". */
export function accentDisplayName(t: TFunction, hex: string): string {
  const preset = accentPresetFor(hex);
  return preset ? accentPresetLabel(t, preset.n) : t("mobile:self.hub.customAccent");
}
