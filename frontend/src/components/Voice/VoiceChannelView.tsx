import React from "react";
import { Circle, VolumeX } from "lucide-react";
import { useAppStore } from "../../stores/appStore";
import { NavigableList } from "../ui/NavigableList";
import type { VoiceParticipant, VoiceConnectionQuality } from "../../types";
import { RemoteUserVolumeSlider } from "./RemoteUserVolumeSlider";
import { Avatar } from "../ui/Avatar";

// Excellent quality is the common case — surfacing it would just be noise on
// every row, so we only show a degraded indicator. Poor and Lost are the
// signals that actually mean "this person is lagging."
function qualityIndicator(quality: VoiceConnectionQuality | undefined): {
  color: string;
  label: string;
} | null {
  switch (quality) {
    case "good":
      return { color: "#facc15", label: "Connection: good" };
    case "poor":
      return { color: "#f97316", label: "Connection: poor" };
    case "lost":
      return { color: "#ef4444", label: "Connection: lost" };
    default:
      return null;
  }
}

export const VoiceChannelView: React.FC = () => {
  const { voiceParticipants, voiceActiveSpeakerIds } = useAppStore();

  return (
    <div
      className="flex-1 flex flex-col font-mono text-xs"
      style={{
        borderTop: "1px solid var(--c-border)",
        borderBottom: "1px solid var(--c-border)",
      }}
    >
      <NavigableList<VoiceParticipant>
        items={voiceParticipants}
        getKey={(p) => p.identity}
        emptyLabel="Connecting…"
        testId="voice-channel-view"
        rowTestId={(p) => `voice-participant-${p.identity}`}
        controls={(p) => {
          // Only remote participants get a per-user volume slider — the
          // mixer applies it before tracks are summed, and the local
          // participant doesn't have an output track on this device.
          if (p.isLocal) {
            return [];
          }
          return [
            <RemoteUserVolumeSlider
              key="volume"
              identity={p.identity}
              participantName={p.name}
            />,
          ];
        }}
        renderRow={(p) => {
          const isSpeaking =
            voiceActiveSpeakerIds.includes(p.identity) && !p.isMuted;
          const nameColor = isSpeaking || p.isLocal
            ? "var(--c-accent)"
            : "var(--c-text)";
          const iconColor = p.isMuted
            ? "var(--c-text-dim)"
            : isSpeaking
            ? "var(--c-accent)"
            : "var(--c-border)";
          return (
            <>
              <span
                data-testid={`voice-speaking-${p.identity}`}
                className={`text-lg ${isSpeaking ? "animate-pulse" : ""}`}
                style={{
                  color: iconColor,
                  lineHeight: 1.25,
                  flexShrink: 0,
                  display: "flex",
                  alignItems: "center",
                  transition: "color 0.1s",
                }}
              >
                {p.isMuted ? (
                  <VolumeX size={16} data-testid={`voice-muted-${p.identity}`} />
                ) : (
                  <Circle
                    size={12}
                    fill={isSpeaking ? "var(--c-accent)" : "var(--c-border)"}
                  />
                )}
              </span>
              <Avatar
                avatarKey={p.avatarKey ?? null}
                size={20}
                alt={p.name}
                testId={`voice-participant-avatar-${p.identity}`}
              />
              <span
                className="flex-1 truncate"
                style={{
                  color: nameColor,
                  opacity: p.isMuted ? 0.6 : 1,
                  transition: "color 0.1s",
                }}
              >
                {p.name}
              </span>
              {(() => {
                const ind = qualityIndicator(p.connectionQuality);
                if (!ind) {
                  return null;
                }
                return (
                  <span
                    data-testid={`voice-quality-${p.identity}`}
                    title={ind.label}
                    aria-label={ind.label}
                    className="flex-shrink-0 flex items-center"
                  >
                    <Circle size={8} fill={ind.color} color={ind.color} />
                  </span>
                );
              })()}
            </>
          );
        }}
      />
    </div>
  );
};
