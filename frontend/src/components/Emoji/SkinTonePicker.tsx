import React from "react";
import { useTranslation } from "react-i18next";
import { SKIN_TONES } from "./emojiData";

interface SkinTonePickerProps {
  toneIndex: number;
  onChange: (toneIndex: number) => void;
}

// The hand each swatch shows. Index 0 is the default (no modifier).
const SWATCH_BASE = "\u{270B}";

/**
 * The six skin-tone swatches, as Discord has them.
 *
 * Inline rather than behind a disclosure: six small buttons cost less room than
 * the popover that would hide them, and a popover inside a popover is exactly
 * the layered-overlay pattern this codebase does not do.
 */
export const SkinTonePicker: React.FC<SkinTonePickerProps> = ({ toneIndex, onChange }) => {
  const { t } = useTranslation("emoji");

  return (
    <div
      data-testid="emoji-skin-tones"
      role="radiogroup"
      aria-label={t("skinTone.groupLabel")}
      className="flex items-center gap-0.5"
    >
      {SKIN_TONES.map((tone, index) => {
        const selected = index === toneIndex;
        return (
          <button
            key={index}
            type="button"
            role="radio"
            aria-checked={selected}
            aria-label={
              index === 0
                ? t("skinTone.default")
                : t("skinTone.numbered", { index })
            }
            data-testid={`emoji-skin-tone-${index}`}
            onClick={() => onChange(index)}
            className="flex items-center justify-center w-5 h-5 text-sm leading-none transition-colors duration-75 hover:bg-hover"
            style={{
              borderRadius: "var(--radius-chip)",
              background: selected ? "var(--c-active)" : undefined,
            }}
          >
            {/* One string, one text node: adjacent JSX expressions become
                separate DOM text nodes, and the font shaper won't combine a
                skin-tone modifier with a base across that boundary — the
                modifier renders as a standalone colored square. */}
            {SWATCH_BASE + tone}
          </button>
        );
      })}
    </div>
  );
};
