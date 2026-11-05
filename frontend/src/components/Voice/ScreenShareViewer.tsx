// Focus mode inside the stage body (no overlay): one stream fills `.vs-body`, closed by the X, Esc or a double-click.

import React from "react";
import { X } from "lucide-react";
import { useTranslation } from "react-i18next";

import { useScreenShareStats } from "../../screenshare/useScreenShareStats";
import { StageTile, type StageTileModel } from "./stage/StageTile";

interface ScreenShareViewerProps {
  /** The stream tile to fill the body with; the stage renders nothing else for its track. */
  tile: StageTileModel;
  onClose: () => void;
}

export const ScreenShareViewer: React.FC<ScreenShareViewerProps> = ({ tile, onClose }) => {
  const { t } = useTranslation("voice");
  const media = tile.media;
  const trackKey = media.kind === "audio" ? null : media.trackKey;
  const stats = useScreenShareStats(trackKey);
  const liveW = stats.dimensions?.width ?? (media.kind === "audio" ? undefined : media.width);
  const liveH = stats.dimensions?.height ?? (media.kind === "audio" ? undefined : media.height);
  const resLabel = liveW && liveH ? `${liveW}×${liveH}` : "";
  const fpsLabel = stats.fps > 0 ? `${stats.fps}fps` : "";
  const label = tile.isLocal ? t("viewer.you") : tile.name;

  return (
    <div className="vs-focus" data-testid="screenshare-viewer">
      <div className="vs-focus-bar">
        <span className="truncate">
          {t("viewer.watching", { name: label })}
          {resLabel ? ` — ${resLabel}` : ""}
          {fpsLabel ? ` · ${fpsLabel}` : ""}
        </span>
        <button
          data-testid="screenshare-viewer-close"
          onClick={onClose}
          aria-label={t("viewer.closeLabel")}
          title={t("viewer.closeTitle")}
          className="vs-focus-close"
        >
          <X size={14} />
        </button>
      </div>
      {/* Double-click anywhere on the stream leaves focus mode (single clicks stay free for the tile). */}
      <div className="vs-focus-main" onDoubleClick={onClose}>
        <StageTile key={tile.tileKey} participant={tile} mode="big" />
      </div>
    </div>
  );
};
