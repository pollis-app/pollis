// A single voice participant cell in the NavigableGrid. Discord-style:
// the tile flips between two states:
//   - Not streaming: avatar centered, name in the top-left
//   - Streaming: low-cost preview (15fps, 2× downsampled) of their
//     share fills the tile; clicking opens the fullscreen viewer
// Speaking shows an accent ring; muted / degraded connection get
// corner indicators. The per-user volume slider lives at the bottom
// *inside* the tile and is revealed when the tile is keyboard-selected
// or hovered (remote only — you don't attenuate yourself).

import React, { useState } from "react";
import { VolumeX } from "lucide-react";

import type { VoiceConnectionQuality } from "../../types";
import { Avatar } from "../ui/Avatar";
import { LoadingSpinner } from "../ui/LoaderSpinner";
import { RemoteUserVolumeSlider } from "./RemoteUserVolumeSlider";
import { RemoteVideoTile } from "./RemoteVideoTile";
import { useScreenShareStats } from "../../screenshare/useScreenShareStats";

interface Props {
  identity: string;
  name: string;
  avatarKey: string | null;
  isMuted: boolean;
  isLocal: boolean;
  isSpeaking: boolean;
  focused: boolean;
  connectionQuality?: VoiceConnectionQuality;
  /** Track key + dimensions of this participant's active screen share.
   *  Defined for both local-and-broadcasting and remote-broadcasting; the
   *  tile renders the preview the same way regardless. Undefined =
   *  not streaming → avatar layout. */
  streamTrackKey?: string;
  streamWidth?: number;
  streamHeight?: number;
  /** Show a subtle connecting indicator (local user only, while the
   *  voice session is in the `joining` phase). Cleared as soon as the
   *  LiveKit connect resolves. */
  isConnecting?: boolean;
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
  streamTrackKey,
  streamWidth,
  streamHeight,
  isConnecting = false,
  onView,
}) => {
  const [hovered, setHovered] = useState(false);
  const showSlider = !isLocal && (focused || hovered);
  const quality = degradedColor(connectionQuality);
  const isStreaming = streamTrackKey !== undefined;
  // Stats overlay for the streaming tile — only fetched when the
  // hook has a key, so the hook returns inert values for non-streamers.
  const stats = useScreenShareStats(streamTrackKey ?? null);

  // Default border is transparent — speaking flips it to the accent
  // color. Focus also uses the accent color so the keyboard cursor stays
  // visible. No box-shadow / glow anywhere on the tile.
  const borderColor = isSpeaking || focused ? "var(--c-accent)" : "transparent";

  // Stats string for the streaming tile: WxH @ Nfps. Drops to the
  // height-only shorthand (720p / 1080p) when it matches a common
  // rung, otherwise uses the raw height — same rule as the old
  // ScreenShareIndicator.
  const statsLabel = (() => {
    if (!isStreaming) {
      return null;
    }
    const w = stats.dimensions?.width ?? streamWidth;
    const h = stats.dimensions?.height ?? streamHeight;
    if (!w || !h) {
      return null;
    }
    const heightLabel = h % 90 === 0 && h <= 4320 ? `${h}p` : `${h}px`;
    const fpsLabel = stats.fps > 0 ? `${stats.fps}fps` : "…";
    return `${heightLabel} ${fpsLabel}`;
  })();

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
        transition: "border-color 0.1s",
      }}
    >
      {/* Top-left: participant name (always present). Sits above the
        * avatar / preview via absolute positioning so the centered
        * content stays vertically centered. When the tile is showing
        * a stream the name renders as a solid accent pill against the
        * video — guarantees contrast over any source content without
        * relying on text-shadow tricks. truncate + max-width clamps a
        * long username so it ellipsizes inside the tile's left bound. */}
      <span
        className="absolute top-1.5 left-2 z-10 truncate text-xs"
        style={
          isStreaming
            ? {
                maxWidth: "calc(100% - 2.25rem)",
                color: "var(--c-bg)",
                background: "var(--c-accent)",
                padding: "0.125rem 0.25rem",
                borderRadius: "0.125rem",
              }
            : {
                maxWidth: "calc(100% - 2.25rem)",
                color: isSpeaking || isLocal ? "var(--c-accent)" : "var(--c-text)",
              }
        }
        title={name}
      >
        {name}
      </span>

      {/* Top-right: connecting spinner (local user only, while
        * LiveKit is negotiating) → muted → degraded connection.
        * Mutually exclusive — once connected, isConnecting flips to
        * false and the muted/quality slot takes over. Spinner uses
        * the existing CLI-style LoadingSpinner so it matches the
        * rest of the app. */}
      {isConnecting ? (
        <span
          data-testid={`voice-tile-connecting-${identity}`}
          className="absolute top-1.5 right-1.5 z-10 flex items-center leading-none"
          title="Connecting…"
          aria-label="Connecting"
        >
          <LoadingSpinner size="sm" />
        </span>
      ) : isMuted ? (
        <span
          data-testid={`voice-tile-muted-${identity}`}
          className="absolute top-1.5 right-1.5 z-10"
          style={{ color: "var(--c-text-dim)" }}
          title={`${name} is muted`}
        >
          <VolumeX size={14} />
        </span>
      ) : quality ? (
        <span
          data-testid={`voice-tile-quality-${identity}`}
          className="absolute top-1.5 right-1.5 z-10 flex items-center"
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
      ) : null}

      {/* Main area — avatar layout OR stream preview. Fills the space
        * above the slider strip. */}
      <div
        className="flex-1 flex flex-col items-center justify-center min-w-0 min-h-0"
        onClick={isStreaming ? () => onView(streamTrackKey!) : undefined}
        style={{
          cursor: isStreaming ? "pointer" : "default",
          // The preview's canvas uses max-width/max-height to fit; the
          // surrounding flex container handles centering for both
          // states.
          padding: isStreaming ? 0 : "0.5rem",
          overflow: "hidden",
        }}
      >
        {isStreaming ? (
          <RemoteVideoTile
            trackKey={streamTrackKey}
            initialWidth={streamWidth}
            initialHeight={streamHeight}
            preview
          />
        ) : (
          <Avatar
            avatarKey={avatarKey}
            size={56}
            alt={name}
            testId={`voice-tile-avatar-${identity}`}
          />
        )}
      </div>

      {/* Stats overlay for streaming tiles (bottom-right corner of the
        * preview). Pulled out into the corner so it never crowds the
        * preview itself and stays legible against any background. */}
      {isStreaming && statsLabel && (
        <span
          data-testid={`voice-tile-stream-stats-${identity}`}
          className="absolute right-1.5 z-10 font-mono text-[10px] tabular-nums pointer-events-none"
          style={{
            color: "var(--c-text)",
            background: "rgba(0,0,0,0.55)",
            padding: "1px 5px",
            borderRadius: 3,
            // Sit above the slider strip on remote tiles (which is
            // always reserved), else flush against the bottom on local.
            bottom: !isLocal ? "calc(1.5rem + 4px)" : "0.375rem",
          }}
        >
          {statsLabel}
        </span>
      )}

      {/* In-tile volume slider strip. Always reserves vertical space on
        * remote tiles so toggling visibility on hover/focus doesn't shift
        * the layout. `visibility: hidden` keeps the slot — `display:
        * none` would collapse it. Local tiles never get a slider (you
        * don't attenuate yourself). */}
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
