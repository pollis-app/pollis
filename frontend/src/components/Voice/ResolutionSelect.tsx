// The outgoing resolution setting (#1902): a height ceiling for the published screen share or camera,
// as a radio list. The same control serves Voice & Video settings and the in-call resolution panel.

import React from "react";
import { useTranslation } from "react-i18next";

import { RadioList } from "../ui/RadioList";
import {
  CAMERA_RESOLUTION_OPTIONS,
  SCREEN_SHARE_RESOLUTION_OPTIONS,
} from "../../voice/videoResolution";

type ResolutionTrack = "share" | "camera";

const TRACKS: Record<
  ResolutionTrack,
  { options: readonly number[]; labelKey: string; hintKey: string; testId: string }
> = {
  share: {
    options: SCREEN_SHARE_RESOLUTION_OPTIONS,
    labelKey: "settings.captureResolution",
    hintKey: "settings.shareResolutionHint",
    testId: "screenshare-resolution",
  },
  camera: {
    options: CAMERA_RESOLUTION_OPTIONS,
    labelKey: "settings.cameraResolution",
    hintKey: "settings.cameraResolutionHint",
    testId: "camera-resolution",
  },
};

interface ResolutionSelectProps {
  track: ResolutionTrack;
  /** One of the track's presets; `0` is the source's own size. */
  value: number;
  onChange: (maxHeight: number) => void;
  /** The settings page explains the trade-off; the in-call panel has no room for it. */
  showHint?: boolean;
}

export const ResolutionSelect: React.FC<ResolutionSelectProps> = ({
  track,
  value,
  onChange,
  showHint = true,
}) => {
  const { t } = useTranslation("voice");
  const { options, labelKey, hintKey, testId } = TRACKS[track];
  return (
    <div className="flex flex-col gap-2 max-w-[20rem]">
      <span className="text-muted">{t(labelKey)}</span>
      <RadioList
        aria-label={t(labelKey)}
        data-testid={testId}
        value={String(value)}
        onChange={(v) => onChange(Number(v))}
        options={options.map((height) => ({
          value: String(height),
          label:
            height === 0
              ? t("settings.resolutionSource")
              : t("settings.resolutionOption", { height }),
        }))}
      />
      {showHint && <span className="text-secondary font-mono text-muted">{t(hintKey)}</span>}
    </div>
  );
};
