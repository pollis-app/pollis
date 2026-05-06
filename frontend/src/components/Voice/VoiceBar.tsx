import React from "react";
import { useNavigate } from "@tanstack/react-router";
import { useVoiceChannel } from "../../hooks/useVoiceChannel";
import { useAppStore } from "../../stores/appStore";
import { useUserGroupsWithChannels } from "../../hooks/queries/useGroups";
import { Volume2, Mic, MicOff, PhoneOff } from "lucide-react";
import { PillButton } from "../ui/PillButton";

interface VoiceBarProps {
  channelId: string;
  channelName: string;
}

export const VoiceBar: React.FC<VoiceBarProps> = ({ channelId, channelName }) => {
  const { voiceParticipants, voiceIsMuted, voiceActiveSpeakerIds, currentUser } = useAppStore();
  const { data: groupsWithChannels } = useUserGroupsWithChannels();
  const navigate = useNavigate();

  const groupId = groupsWithChannels?.find((g) =>
    g.channels.some((c) => c.id === channelId)
  )?.id ?? null;

  const { toggleMute, leave } = useVoiceChannel(channelId, groupId);

  // Local participant identity is `voice-${userId}` (see useVoiceChannel.ts).
  // The voice bar is feedback about *other* speakers, so always exclude self.
  const localIdentity = currentUser ? `voice-${currentUser.id}` : null;
  const remoteActiveSpeakerIds = voiceActiveSpeakerIds.filter((id) => id !== localIdentity);
  const lastRemoteSpeakerId = remoteActiveSpeakerIds.at(-1);

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
      <PillButton
        data-testid="voice-bar-channel-name"
        accent="var(--c-accent)"
        onClick={() => {
          if (channelId.startsWith("call-")) {
            const callId = channelId.slice("call-".length);
            navigate({ to: "/call/$callId", params: { callId } });
          } else if (groupId) {
            navigate({ to: "/groups/$groupId/voice/$channelId", params: { groupId, channelId } });
          }
        }}
        title={
          channelId.startsWith("call-")
            ? "Return to call"
            : `Go to ${channelName} voice channel`
        }
      >
        <Volume2 size={12} />
        {channelName}
      </PillButton>

      {/* Mute toggle */}
      <PillButton
        data-testid="voice-bar-mute-button"
        accent={voiceIsMuted ? "#ff6b6b" : "var(--c-accent)"}
        onClick={toggleMute}
        title={voiceIsMuted ? "Unmute microphone" : "Mute microphone"}
        aria-label={voiceIsMuted ? "Unmute microphone" : "Mute microphone"}
        square
      >
        {voiceIsMuted ? <MicOff size={12} /> : <Mic size={12} />}
      </PillButton>

      {/* Leave button */}
      <PillButton
        data-testid="voice-bar-leave-button"
        accent="#ff6b6b"
        onClick={leave}
        title="Leave voice channel"
        aria-label="Leave voice channel"
        square
      >
        <PhoneOff size={12} />
      </PillButton>

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
        {lastRemoteSpeakerId
          ? <>
            <Volume2 size={12} style={{ verticalAlign: "middle" }} />
            {voiceParticipants.find(p => p.identity === lastRemoteSpeakerId)?.name}
          </>
          : null}
      </span>
    </div>
  );
};
