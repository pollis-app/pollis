// The "share resolution" setting: a height ceiling for the published screen share.

import React from "react";
import { useTranslation } from "react-i18next";

import { Button } from "../ui/Button";
import { SCREEN_SHARE_RESOLUTION_OPTIONS } from "../../hooks/queries/usePreferences";

interface ShareResolutionSelectProps {
  /** One of `SCREEN_SHARE_RESOLUTION_OPTIONS`; `0` is the source's own size. */
  value: number;
  onChange: (maxHeight: number) => void;
}

export const ShareResolutionSelect: React.FC<ShareResolutionSelectProps> = ({ value, onChange }) => {
  const { t } = useTranslation("voice");
  return (
    <div className="flex flex-col gap-2" style={{ maxWidth: 320 }}>
      <span className="text-muted">{t("settings.captureResolution")}</span>
      <div className="flex gap-2">
        {SCREEN_SHARE_RESOLUTION_OPTIONS.map((height) => (
          <Button
            key={height}
            data-testid={`screenshare-resolution-${height}`}
            variant={value === height ? "primary" : "secondary"}
            size="sm"
            onClick={() => onChange(height)}
          >
            {height === 0
              ? t("settings.resolutionSource")
              : t("settings.resolutionOption", { height })}
          </Button>
        ))}
      </div>
      <span className="text-xs font-mono text-muted">{t("settings.resolutionHint")}</span>
    </div>
  );
};
