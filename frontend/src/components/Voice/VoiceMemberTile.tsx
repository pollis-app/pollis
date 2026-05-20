// A single voice participant cell in the NavigableGrid. Discord-style:
// avatar centered in a rounded rectangle that grows/shrinks with the
// grid. Speaking shows an accent ring; muted / degraded-connection get
// corner indicators. A broadcasting participant gets the existing "View
// Stream" affordance (opens the full-window ScreenShareViewer). The
// per-user volume slider lives at the bottom *inside* the tile and is
// revealed when the tile is keyboard-selected or hovered (remote only —
// you don't attenuate yourself).

import React, { useState } from "react";
import { Play, Radio, VolumeX } from "lucide-react";

import type { VoiceConnectionQuality } from "../../types";
import { Avatar } from "../ui/Avatar";
import { PillButton } from "../ui/PillButton";
import { RemoteUserVolumeSlider } from "./RemoteUserVolumeSlider";

interface Props {
  identity: string;
  name: string;
  avatarKey: string | null;
  isMuted: boolean;
  isLocal: boolean;
  isSpeaking: boolean;
  focused: boolean;
  connectionQuality?: VoiceConnectionQuality;
  remoteShare?: { trackKey: string; width: number; height: number };
  isLocalBroadcasting: boolean;
  onView: (trackKey: string) => void;
}

// Excellent is the common case and would just be noise; only surface a
// degraded link.
function degradedColor(q: VoiceConnectionQuality | undefined): string | null {
  switch (q) {
    case "good":
      return "#facc15";
    case "poor":
      return "#f97316";
    case "lost":
      return "#ef4444";
    default:
      return null;
  }
}

export const VoiceMemberTile: React.FC<Props> = ({
  identity,
  name,
  avatarKey,
  isMuted,
  isLocal,
  isSpeaking,
  focused,
  connectionQuality,
  remoteShare,
  isLocalBroadcasting,
  onView,
}) => {
  const [hovered, setHovered] = useState(false);
  const showSlider = !isLocal && (focused || hovered);
  const quality = degradedColor(connectionQuality);

  const borderColor = isSpeaking
    ? "var(--c-accent)"
    : focused
    ? "var(--c-border-active)"
    : "var(--c-border)";

  // Speaking = solid accent ring/glow (not a pulse — pulsing the whole
  // tile dims the avatar and the volume slider mid-drag). Focus adds its
  // own accent outline; speaking wins when both apply.
  const boxShadow = isSpeaking
    ? "0 0 0 2px var(--c-accent), 0 0 12px -2px var(--c-accent)"
    : focused
    ? "0 0 0 2px var(--c-accent)"
    : undefined;

  return (
    <div
      data-testid={`voice-tile-${identity}`}
      onMouseEnter={() => setHovered(true)}
      onMouseLeave={() => setHovered(false)}
      className="relative w-full h-full flex flex-col select-none overflow-hidden font-mono"
      style={{
        background: "var(--c-surface)",
        border: `2px solid ${borderColor}`,
        borderRadius: 10,
        boxShadow,
        transition: "border-color 0.1s, box-shadow 0.1s",
      }}
    >
      {/* Top-left: muted */}
      {isMuted && (
        <span
          data-testid={`voice-tile-muted-${identity}`}
          className="absolute top-1.5 left-1.5"
          style={{ color: "var(--c-text-dim)" }}
          title={`${name} is muted`}
        >
          <VolumeX size={14} />
        </span>
      )}

      {/* Top-right: degraded connection */}
      {quality && (
        <span
          data-testid={`voice-tile-quality-${identity}`}
          className="absolute top-1.5 right-1.5 flex items-center"
          title={`Connection: ${connectionQuality}`}
        >
          <span
            style={{
              width: 8,
              height: 8,
              borderRadius: "50%",
              background: quality,
              display: "block",
            }}
          />
        </span>
      )}

      {/* Local broadcasting badge */}
      {isLocal && isLocalBroadcasting && (
        <span
          data-testid={`voice-tile-live-${identity}`}
          className="absolute top-1.5 right-1.5 flex items-center gap-1 text-[10px]"
          style={{
            color: "var(--c-accent)",
            padding: "1px 5px",
            border: "1px solid var(--c-accent)",
            borderRadius: 3,
            letterSpacing: "0.05em",
          }}
          title="You are sharing your screen"
        >
          <Radio size={9} className="animate-pulse" />
          LIVE
        </span>
      )}

      {/* Centered identity — fills the area above the slider strip */}
      <div className="flex-1 flex flex-col items-center justify-center gap-2 min-w-0 px-2">
        <Avatar
          avatarKey={avatarKey}
          size={56}
          alt={name}
          testId={`voice-tile-avatar-${identity}`}
        />
        <span
          className="max-w-full truncate text-xs"
          style={{ color: isSpeaking || isLocal ? "var(--c-accent)" : "var(--c-text)" }}
        >
          {name}
          {isLocal ? " (you)" : ""}
        </span>

        {/* View Stream — only for a remote participant who is broadcasting */}
        {remoteShare && (
          <PillButton
            accent="var(--c-accent)"
            data-testid={`voice-tile-view-stream-${identity}`}
            aria-label={`View screen share from ${name}`}
            title={`View ${remoteShare.width}×${remoteShare.height} screen share`}
            onClick={() => onView(remoteShare.trackKey)}
          >
            <Play size={10} fill="currentColor" />
            <span className="text-[10px]">View Stream</span>
          </PillButton>
        )}
      </div>

      {/* In-tile volume slider strip. Always reserves vertical space on
        * remote tiles so toggling visibility on hover/focus doesn't shift
        * the View Stream button. `visibility: hidden` keeps the slot —
        * `display: none` would collapse it. Local tiles never get a
        * slider (you don't attenuate yourself), so no strip reserved. */}
      {!isLocal && (
        <div
          data-testid={`voice-tile-volume-${identity}`}
          className="flex items-center justify-center py-1.5 flex-shrink-0"
          style={{
            background: "var(--c-bg)",
            borderTop: "1px solid var(--c-border)",
            visibility: showSlider ? "visible" : "hidden",
          }}
        >
          <RemoteUserVolumeSlider identity={identity} participantName={name} />
        </div>
      )}
    </div>
  );
};
