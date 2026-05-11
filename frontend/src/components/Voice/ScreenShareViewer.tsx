// Inline viewer for a remote participant's screen share. Mounted at
// AppShell level so it covers the route view but stays inside the app
// chrome (TitleBar / Breadcrumb / VoiceBar). Closing returns the user to
// whatever they were doing.

import React from "react";
import { X } from "lucide-react";

import { useAppStore } from "../../stores/appStore";
import { RemoteVideoTile } from "./RemoteVideoTile";

export const ScreenShareViewer: React.FC = () => {
  const {
    viewingScreenShareTrackKey,
    screenShareRemotes,
    setViewingScreenShareTrackKey,
  } = useAppStore();
  if (!viewingScreenShareTrackKey) {
    return null;
  }
  const entry = Object.entries(screenShareRemotes).find(
    ([, info]) => info.trackKey === viewingScreenShareTrackKey,
  );
  if (!entry) {
    return null;
  }
  const [identity, info] = entry;
  return (
    <div
      data-testid="screenshare-viewer"
      style={{
        position: "absolute",
        inset: 0,
        zIndex: 8000,
        background: "rgba(0,0,0,0.92)",
        display: "flex",
        flexDirection: "column",
      }}
    >
      <div
        className="flex items-center justify-between px-3 font-mono text-xs"
        style={{
          height: 28,
          color: "var(--c-text-muted)",
          borderBottom: "1px solid var(--c-border)",
          background: "var(--c-surface)",
        }}
      >
        <span>watching {identity.replace(/^voice-/, "")} — {info.width}×{info.height}</span>
        <button
          data-testid="screenshare-viewer-close"
          onClick={() => setViewingScreenShareTrackKey(null)}
          aria-label="Close stream"
          title="Close stream (Esc)"
          style={{
            background: "none",
            border: "none",
            padding: 0,
            color: "var(--c-text-muted)",
            cursor: "pointer",
            display: "flex",
            alignItems: "center",
          }}
        >
          <X size={14} />
        </button>
      </div>
      <div style={{ flex: 1, minHeight: 0, display: "flex", justifyContent: "center", alignItems: "center" }}>
        <RemoteVideoTile
          trackKey={info.trackKey}
          initialWidth={info.width}
          initialHeight={info.height}
        />
      </div>
    </div>
  );
};
