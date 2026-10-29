import React from "react";
import { useTranslation } from "react-i18next";
import { SKIN_TONES } from "./emojiData";

interface SkinTonePickerProps {
  toneIndex: number;
  onChange: (toneIndex: number) => void;
}

// Index 0 is the default (no modifier).
const SWATCH_BASE = "\u{270B}";

/** Six inline skin-tone swatches (no popover-in-popover). */
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
            className="flex items-center justify-center w-5 h-5 text-sm leading-none hover:bg-hover rounded-chip"
            style={{
              background: selected ? "var(--c-active)" : undefined,
            }}
          >
            {/* One text node: the shaper won't join a modifier across nodes. */}
            {SWATCH_BASE + tone}
          </button>
        );
      })}
    </div>
  );
};
