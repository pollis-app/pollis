// In-call resolution panel (#1902): docked above the tray, in the stage's own flow (no overlay), it
// holds the resolution of each stream the user is sending right now. The same preferences as Voice &
// Video settings; a pick moves the running stream at once.

import React from "react";
import { useTranslation } from "react-i18next";

import { ResolutionSelect } from "../ResolutionSelect";
import { useOutgoingResolution } from "../../../hooks/useOutgoingResolution";

interface ResolutionPanelProps {
  shareActive: boolean;
  cameraActive: boolean;
}

export const ResolutionPanel: React.FC<ResolutionPanelProps> = ({ shareActive, cameraActive }) => {
  const { t } = useTranslation("voice");
  const resolution = useOutgoingResolution();
  return (
    <section
      aria-label={t("stage.resolutionPanel")}
      data-testid="voice-resolution-panel"
      className="flex-shrink-0 flex flex-wrap gap-x-8 gap-y-4 px-4 py-3 max-h-[45vh] overflow-y-auto bg-surface"
    >
      {shareActive && (
        <ResolutionSelect
          track="share"
          value={resolution.shareMaxHeight}
          onChange={resolution.setShareMaxHeight}
          showHint={false}
        />
      )}
      {cameraActive && (
        <ResolutionSelect
          track="camera"
          value={resolution.cameraMaxHeight}
          onChange={resolution.setCameraMaxHeight}
          showHint={false}
        />
      )}
    </section>
  );
};
