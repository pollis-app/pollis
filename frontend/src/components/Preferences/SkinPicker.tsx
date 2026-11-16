import React from "react";
import { useTranslation } from "react-i18next";
import { RadioList } from "../ui/RadioList";
import { normalizeSkin, type Skin } from "../../utils/colorUtils";

interface SkinPickerProps {
  value: Skin;
  onChange: (skin: Skin) => void;
  /** Rows get `<data-testid>-<skin>`, e.g. `pref-skin-modern`. */
  "data-testid": string;
}

/** The Terminal / Modern skin choice, shared by Preferences and the first-run step (#1924). */
export const SkinPicker: React.FC<SkinPickerProps> = ({ value, onChange, "data-testid": testId }) => {
  const { t } = useTranslation("settings");
  return (
    <RadioList
      aria-label={t("appearance.ariaLabel")}
      data-testid={testId}
      value={value}
      onChange={(next) => onChange(normalizeSkin(next))}
      options={[
        { value: "modern", label: t("appearance.skinModern"), detail: t("appearance.skinModernDetail") },
        { value: "terminal", label: t("appearance.skinTerminal"), detail: t("appearance.skinTerminalDetail") },
      ]}
    />
  );
};
