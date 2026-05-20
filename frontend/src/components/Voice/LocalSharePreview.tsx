// Small self-preview of the user's own outgoing screen share. The backend
// mirrors the local capture under LOCAL_PREVIEW_KEY at a low frame rate so
// the sharer can confirm video is actually being sent — it is not meant to
// be a full-fidelity view. Rendered inline (no overlay/modal) beneath the
// voice participant list while sharing is active.

import React from "react";

import { useAppStore } from "../../stores/appStore";
import { RemoteVideoTile } from "./RemoteVideoTile";
import { LOCAL_PREVIEW_KEY } from "../../screenshare/screenShareSession";

export const LocalSharePreview: React.FC = () => {
  const { screenShareLocalActive } = useAppStore();
  if (!screenShareLocalActive) {
    return null;
  }
  return (
    <div
      data-testid="local-share-preview"
      className="flex flex-col gap-1 px-3 py-2 flex-shrink-0"
      style={{ borderTop: "1px solid var(--c-border)" }}
    >
      <span
        className="font-mono text-[10px]"
        style={{ color: "var(--c-text-dim)", letterSpacing: "0.05em" }}
      >
        YOUR STREAM
      </span>
      <div
        style={{
          position: "relative",
          width: 192,
          height: 108,
          background: "#000",
          border: "1px solid var(--c-border)",
          display: "flex",
          alignItems: "center",
          justifyContent: "center",
          overflow: "hidden",
        }}
      >
        <RemoteVideoTile trackKey={LOCAL_PREVIEW_KEY} />
      </div>
    </div>
  );
};
