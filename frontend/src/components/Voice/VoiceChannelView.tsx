import React from "react";
import { useAppStore } from "../../stores/appStore";

export const VoiceChannelView: React.FC = () => {
  const { voiceParticipants, voiceActiveSpeakerIds, activeVoiceChannelId } = useAppStore();

  return (
    <div
      data-testid="voice-channel-view"
      className="flex flex-col flex-1 font-mono text-xs overflow-hidden"
      style={{ background: "var(--c-bg)", color: "var(--c-text)" }}
    >
      {/* Divider */}
      <div
        className="mx-4 mt-1 mb-1 flex-shrink-0"
        style={{ borderBottom: "1px solid var(--c-border)" }}
      />

      {/* Participant list */}
      <div
        data-testid="voice-participant-list"
        className="flex-1 overflow-auto px-4 py-2 flex flex-col gap-1"
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
              {/* Speaking pulse dot */}
              <span
                data-testid={`voice-speaking-${p.identity}`}
                className={isSpeaking ? "animate-pulse" : ""}
                style={{
                  color: isSpeaking ? "var(--c-accent)" : "var(--c-border)",
                  fontSize: "0.6rem",
                  lineHeight: 1,
                  flexShrink: 0,
                }}
              >
                ●
              </span>

              {/* Username */}
              <span className="flex-1 truncate">
                {p.name}
                {p.isLocal && (
                  <span style={{ color: "var(--c-text-dim)" }}> (you)</span>
                )}
              </span>

              {/* Muted indicator */}
              {p.isMuted && (
                <span
                  data-testid={`voice-muted-${p.identity}`}
                  style={{ color: "var(--c-text-dim)" }}
                  title="Muted"
                >
                  [m]
                </span>
              )}
            </div>
          );
        })}
      </div>

      {/* Bottom divider */}
      <div
        className="mx-4 mb-2 flex-shrink-0"
        style={{ borderTop: "1px solid var(--c-border)" }}
      />

      {/* Status note */}
      {activeVoiceChannelId && (
        <div
          className="px-4 pb-3 flex-shrink-0 text-xs"
          style={{ color: "var(--c-text-dim)" }}
        >
          Use the bar below to mute or leave.
        </div>
      )}
    </div>
  );
};
