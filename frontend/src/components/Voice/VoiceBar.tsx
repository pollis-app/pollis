import React from "react";
import { useNavigate } from "@tanstack/react-router";
import { useAppStore } from "../../stores/appStore";
import { useUserGroupsWithChannels } from "../../hooks/queries/useGroups";
import { Volume2, Mic, MicOff, PhoneOff, SlidersHorizontal, Monitor, MonitorOff } from "lucide-react";
import { PillButton } from "../ui/PillButton";
import { voiceSession } from "../../voice";
import {
  friendlyScreenShareError,
  screenShareSession,
} from "../../screenshare/screenShareSession";

interface VoiceBarProps {
  channelId: string;
  channelName: string;
}

export const VoiceBar: React.FC<VoiceBarProps> = ({ channelId, channelName }) => {
  const {
    voiceParticipants,
    voiceIsMuted,
    voiceActiveSpeakerIds,
    currentUser,
    screenShareLocalActive,
    screenShareMode,
  } = useAppStore();
  const { data: groupsWithChannels } = useUserGroupsWithChannels();
  const navigate = useNavigate();

  const groupId = groupsWithChannels?.find((g) =>
    g.channels.some((c) => c.id === channelId)
  )?.id ?? null;

  const toggleMute = () => voiceSession.toggleMute();
  const leave = () => voiceSession.leave();

  // The voice bar is feedback about *other* speakers, so always exclude self.
  // Local participant identity is `voice-${userId}` (see VoiceSessionManager).
  const localIdentity = currentUser ? `voice-${currentUser.id}` : null;
  const remoteActiveSpeakerIds = voiceActiveSpeakerIds.filter((id) => id !== localIdentity);
  const lastRemoteSpeakerId = remoteActiveSpeakerIds.at(-1);

  return (
    <div
      data-testid="voice-bar"
      className="flex items-center pl-1 pr-3 gap-2 font-mono text-xs flex-shrink-0"
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
        accent={voiceIsMuted ? "var(--c-danger)" : "var(--c-accent)"}
        onClick={toggleMute}
        title={voiceIsMuted ? "Unmute microphone" : "Mute microphone"}
        aria-label={voiceIsMuted ? "Unmute microphone" : "Mute microphone"}
        square
      >
        {voiceIsMuted ? <MicOff size={12} /> : <Mic size={12} />}
      </PillButton>

      {/* Screen share toggle. On macOS we enumerate via SCShareableContent
          and route to our in-app picker — the system picker
          (SCContentSharingPicker) has an upstream crate bug that crashes
          on selection (#283), so we use the same enumerate-and-pick
          flow Slack/Discord/Zoom do. On Linux/Windows the helper falls
          back to the system portal / WGC picker — the backend signals
          this by returning an empty source list from enumerate(). */}
      <PillButton
        data-testid="voice-bar-screenshare-button"
        accent={
          screenShareLocalActive || screenShareMode !== "idle"
            ? "var(--c-danger)"
            : "var(--c-accent)"
        }
        onClick={() => {
          if (screenShareLocalActive) {
            screenShareSession
              .stop()
              .catch((e) => console.error("[screenshare] stop", e));
            return;
          }
          if (screenShareMode === "picking") {
            // Button doubles as a cancel affordance while the picker is open.
            screenShareSession
              .cancelPicker()
              .catch((e) => console.warn("[screenshare] cancel:", e))
              .finally(() => {
                useAppStore.getState().setScreenShareMode("idle");
                useAppStore.getState().setScreenShareSources(null);
              });
            return;
          }
          // Engage enumerate→pick→start. The backend returns an empty
          // list on Linux/Windows; in that case we skip our picker and
          // go straight to start() (system portal/WGC handles selection).
          (async () => {
            try {
              const list = await screenShareSession.enumerate();
              if (list.displays.length + list.windows.length === 0) {
                await screenShareSession.start();
                return;
              }
              useAppStore.getState().setScreenShareSources(list);
              useAppStore.getState().setScreenShareMode("picking");
            } catch (e) {
              console.error("[screenshare] enumerate:", e);
              useAppStore
                .getState()
                .setScreenShareError(friendlyScreenShareError(String(e)));
            }
          })();
        }}
        title={
          screenShareLocalActive
            ? "Stop screen share"
            : screenShareMode === "picking"
              ? "Cancel"
              : "Go live (share screen)"
        }
        aria-label={
          screenShareLocalActive
            ? "Stop screen share"
            : screenShareMode === "picking"
              ? "Cancel screen share picker"
              : "Share screen"
        }
        square
      >
        {screenShareLocalActive ? <MonitorOff size={12} /> : <Monitor size={12} />}
      </PillButton>

      {/* Leave button */}
      <PillButton
        data-testid="voice-bar-leave-button"
        accent="var(--c-danger)"
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

      {/* Voice settings shortcut */}
      <button
        data-testid="voice-bar-settings-button"
        onClick={() => navigate({ to: "/voice-settings" })}
        aria-label="Voice settings"
        title="Voice settings"
        className="flex items-center justify-center transition-colors flex-shrink-0 text-[var(--c-text-muted)] hover:text-[var(--c-accent)]"
        style={{
          width: 20,
          height: 20,
          background: "none",
          border: "none",
          padding: 0,
          cursor: "pointer",
        }}
      >
        <SlidersHorizontal size={14} />
      </button>
    </div>
  );
};
