// Fullscreen viewer for a participant's screen share. Mounted at the
// AppShell level so it covers the route view but stays inside the app
// chrome (TitleBar / Breadcrumb / VoiceBar). The viewer toggles with
// `viewingScreenShareTrackKey`: a participant tile sets it on click,
// and either the X button OR a click anywhere on the video area
// clears it (returning to the in-tile preview).

import React from "react";
import { X } from "lucide-react";

import { observer } from "mobx-react-lite";
import { appStore } from "../../stores/appStore";
import { RemoteVideoTile } from "./RemoteVideoTile";
import { LOCAL_PREVIEW_KEY } from "../../screenshare/screenShareSession";
import { useScreenShareStats } from "../../screenshare/useScreenShareStats";
import { shareOf } from "../../types/voice-state";

export const ScreenShareViewer: React.FC = observer(() => {
  const {
    viewingScreenShareTrackKey,
    voiceParticipants,
    voiceState,
    currentUser,
    setViewingScreenShareTrackKey,
  } = appStore;
  const share = shareOf(voiceState);
  const screenShareLocalActive = share.kind === 'active';
  const screenShareLocalDimensions = share.kind === 'active' ? share.dimensions : null;
  // Live frame stats for the viewed track (fps + dimensions). Keyed by the
  // same track key the tile uses; inert when nothing is being viewed.
  const stats = useScreenShareStats(viewingScreenShareTrackKey);
  if (!viewingScreenShareTrackKey) {
    return null;
  }
  // Resolve who/what we're viewing. Local stream uses the reserved
  // LOCAL_PREVIEW_KEY sentinel; remote streams live on the publishing
  // participant's `video` (#385).
  let label: string;
  let trackKey: string;
  let width: number | undefined;
  let height: number | undefined;
  if (viewingScreenShareTrackKey === LOCAL_PREVIEW_KEY) {
    if (!screenShareLocalActive) {
      return null;
    }
    trackKey = LOCAL_PREVIEW_KEY;
    label = currentUser?.username ?? "you";
    width = screenShareLocalDimensions?.width;
    height = screenShareLocalDimensions?.height;
  } else {
    const p = voiceParticipants.find(
      (p) =>
        p.video.kind === "screenshare" &&
        p.video.trackKey === viewingScreenShareTrackKey,
    );
    if (!p || p.video.kind !== "screenshare") {
      return null;
    }
    label = p.identity.replace(/^voice-/, "");
    trackKey = p.video.trackKey;
    width = p.video.width;
    height = p.video.height;
  }
  // Prefer the live stats dimensions/fps; fall back to the static hints.
  const liveW = stats.dimensions?.width ?? width;
  const liveH = stats.dimensions?.height ?? height;
  const resLabel = liveW && liveH ? `${liveW}×${liveH}` : "";
  const fpsLabel = stats.fps > 0 ? `${stats.fps}fps` : "";
  const close = () => setViewingScreenShareTrackKey(null);
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
        <span>
          watching {label}
          {resLabel ? ` — ${resLabel}` : ""}
          {fpsLabel ? ` · ${fpsLabel}` : ""}
        </span>
        <button
          data-testid="screenshare-viewer-close"
          onClick={close}
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
      {/* Click anywhere on the video area closes the viewer, matching
        * the X. No nested interactive elements inside RemoteVideoTile,
        * so a plain onClick on the wrapper is unambiguous. */}
      <div
        onClick={close}
        role="button"
        aria-label="Close stream"
        style={{
          flex: 1,
          minHeight: 0,
          display: "flex",
          justifyContent: "center",
          alignItems: "center",
          cursor: "pointer",
        }}
      >
        <RemoteVideoTile
          trackKey={trackKey}
          initialWidth={width}
          initialHeight={height}
        />
      </div>
    </div>
  );
});
