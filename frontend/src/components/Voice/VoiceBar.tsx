import React from "react";
import { useNavigate } from "@tanstack/react-router";
import { useVoiceChannel } from "../../hooks/useVoiceChannel";
import { useAppStore } from "../../stores/appStore";
import { useUserGroupsWithChannels } from "../../hooks/queries/useGroups";
import { Volume2 } from "lucide-react";

interface VoiceBarProps {
  channelId: string;
  channelName: string;
}

export const VoiceBar: React.FC<VoiceBarProps> = ({ channelId, channelName }) => {
  const { toggleMute, leave } = useVoiceChannel(channelId);
  const { voiceParticipants, voiceIsMuted, voiceActiveSpeakerIds } = useAppStore();
  const { data: groupsWithChannels } = useUserGroupsWithChannels();
  const navigate = useNavigate();

  const groupId = groupsWithChannels?.find((g) =>
    g.channels.some((c) => c.id === channelId)
  )?.id;

  console.log(voiceActiveSpeakerIds, voiceParticipants);
  return (
    <div
      data-testid="voice-bar"
      className="flex items-center px-3 gap-2 font-mono text-xs flex-shrink-0"
      style={{
        height: 28,
        borderTop: "1px solid var(--c-border)",
        background: "var(--c-surface)",
        color: "var(--c-text-muted)",
      }}
    >
      {/* Channel name */}
      <button
        data-testid="voice-bar-channel-name"
        onClick={() => {
          if (groupId) {
            navigate({ to: "/groups/$groupId/voice/$channelId", params: { groupId, channelId } });
          }
        }}
        // style={{ color: "var(--c-text)" }}
        // onMouseEnter={(e) => { (e.currentTarget as HTMLElement).style.color = "var(--c-accent)"; }}
        // onMouseLeave={(e) => { (e.currentTarget as HTMLElement).style.color = "var(--c-text)"; }}
        className="flex items-center gap-1.5 text-[var(--c-accent)] hover:text-[var(--c-text)] transition-colors"
        title={`Go to ${channelName} voice channel`}
      >
        <Volume2 size={12} />
        {channelName}
      </button>

      <span style={{ color: "var(--c-border)" }}>|</span>

      {/* Mute toggle */}
      <button
        data-testid="voice-bar-mute-button"
        onClick={toggleMute}
        className="transition-colors"
        style={{ color: voiceIsMuted ? "#ff6b6b" : "var(--c-accent)" }}
        onMouseEnter={(e) => {
          (e.currentTarget as HTMLElement).style.opacity = "0.7";
        }}
        onMouseLeave={(e) => {
          (e.currentTarget as HTMLElement).style.opacity = "1";
        }}
        title={voiceIsMuted ? "Unmute microphone" : "Mute microphone"}
      >
        {voiceIsMuted ? "[mic off]" : "[mic on]"}
      </button>

      {/* Leave button */}
      <button
        data-testid="voice-bar-leave-button"
        onClick={leave}
        className="transition-colors"
        style={{ color: "var(--c-text-dim)" }}
        onMouseEnter={(e) => {
          (e.currentTarget as HTMLElement).style.color = "#ff6b6b";
        }}
        onMouseLeave={(e) => {
          (e.currentTarget as HTMLElement).style.color = "var(--c-text-dim)";
        }}
        title="Leave voice channel"
      >
        [leave]
      </button>

      <span style={{ color: "var(--c-border)" }}>|</span>

      {/* Participant count */}
      <span data-testid="voice-bar-participant-count" style={{ color: "var(--c-text-dim)" }}>
        {voiceParticipants.length} participant{voiceParticipants.length !== 1 ? "s" : ""}
      </span>

      {/* Security indicator — audio is transport-encrypted (TLS) but not E2EE for v1 */}
      <span
        data-testid="voice-bar-security-indicator"
        style={{ marginLeft: "auto", color: "var(--c-text-dim)" }}
        className="flex items-center gap-1"
      >
        {voiceActiveSpeakerIds.length > 0
          ? <>
            <Volume2 size={12} style={{ verticalAlign: "middle" }} />
            {voiceParticipants.find(p => voiceActiveSpeakerIds.at(-1) === p.identity)?.name}
          </>
          : null}
      </span>
    </div>
  );
};
