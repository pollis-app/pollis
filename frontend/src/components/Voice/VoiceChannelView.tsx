import React from "react";
import { Circle, VolumeX } from "lucide-react";
import { useAppStore } from "../../stores/appStore";

export const VoiceChannelView: React.FC = () => {
  const { voiceParticipants, voiceActiveSpeakerIds } = useAppStore();

  return (
    <div
      data-testid="voice-channel-view"
      className="flex-1 overflow-auto px-4 py-2 flex flex-col gap-1 font-mono text-xs"
      style={{
        borderTop: "1px solid var(--c-border)",
        borderBottom: "1px solid var(--c-border)",
        color: "var(--c-text)",
      }}
    >
      {voiceParticipants.length === 0 && (
        <span style={{ color: "var(--c-text-dim)" }}>Connecting…</span>
      )}
      {voiceParticipants.map((p) => {
        const isSpeaking = voiceActiveSpeakerIds.includes(p.identity) && !p.isMuted;
        return (
          <div
            key={p.identity}
            data-testid={`voice-participant-${p.identity}`}
            className="flex items-center gap-2"
            style={{
              color: isSpeaking ? "var(--c-accent)" : p.isLocal ? "var(--c-accent)" : "var(--c-text)",
              borderLeft: isSpeaking ? "2px solid var(--c-accent)" : "2px solid transparent",
              paddingLeft: "6px",
              transition: "border-color 0.1s, color 0.1s",
              opacity: p.isMuted ? 0.6 : 1,
            }}
          >
            <span
              data-testid={`voice-speaking-${p.identity}`}
              className={`text-lg ${isSpeaking ? "animate-pulse" : ""}`}
              style={{
                color: p.isMuted ? "var(--c-text-dim)" : isSpeaking ? "var(--c-accent)" : "var(--c-border)",
                lineHeight: 1.25,
                flexShrink: 0,
                display: "flex",
                alignItems: "center",
              }}
            >
              {p.isMuted
                ? <VolumeX size={16} data-testid={`voice-muted-${p.identity}`} />
                : <Circle size={12} fill={isSpeaking ? "var(--c-accent)" : "var(--c-border)"} />
              }
            </span>
            <span className="flex-1 truncate">{p.name}</span>
          </div>
        );
      })}
    </div>
  );
};
