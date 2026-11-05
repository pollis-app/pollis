// Four-bar connection-quality glyph shared by the stage tiles, the voice bar and the sidebar strip (Discord's signal icon).

import React from "react";
import { useTranslation } from "react-i18next";

import type { VoiceConnectionQuality } from "../../types";

/** The drawn class for a quality; `unknown` (no event yet) is dim, never a confident "excellent". */
export function signalClass(q: VoiceConnectionQuality | undefined): "unknown" | "good" | "fair" | "poor" {
  switch (q) {
    case "excellent":
      return "good";
    case "good":
      return "fair";
    case "poor":
    case "lost":
      return "poor";
    default:
      return "unknown";
  }
}

interface SignalBarsProps {
  quality: VoiceConnectionQuality | undefined;
  testId?: string;
  /** Extra classes on the wrapping tag (e.g. `vs-pad6` in the tile corner). */
  className?: string;
}

/** Renders `.vs-tag > .vs-sig` (styles in `stage/voice-stage.css`) with a translated title. */
export const SignalBars: React.FC<SignalBarsProps> = ({ quality, testId, className }) => {
  const { t } = useTranslation("voice");
  // Literal keys so `scripts/i18n-check.mjs` resolves them; the enum is a wire value.
  const label =
    quality === "excellent"
      ? t("tile.qualityExcellent")
      : quality === "good"
        ? t("tile.qualityGood")
        : quality === "poor"
          ? t("tile.qualityPoor")
          : quality === "lost"
            ? t("tile.qualityLost")
            : t("tile.qualityUnknown");
  return (
    <span
      className={"vs-tag" + (className ? ` ${className}` : "")}
      data-testid={testId}
      data-quality={quality ?? "unknown"}
      title={t("tile.quality", { quality: label })}
      aria-label={t("tile.quality", { quality: label })}
    >
      <span className={"vs-sig " + signalClass(quality)}>
        <i /><i /><i /><i />
      </span>
    </span>
  );
};
