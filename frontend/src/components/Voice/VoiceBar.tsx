import React from "react";
import { useNavigate } from "@tanstack/react-router";
import { observer } from "mobx-react-lite";
import { appStore } from "../../stores/appStore";
import { useUserGroupsWithChannels } from "../../hooks/queries/useGroups";
import { Volume2, Mic, MicOff, PhoneOff, SlidersHorizontal, Monitor, MonitorOff } from "lucide-react";
import { PillButton } from "../ui/PillButton";
import { voiceSession } from "../../voice";
import { disambiguateVoiceNames } from "../../voice/disambiguateNames";
import {
  friendlyScreenShareError,
  screenShareSession,
} from "../../screenshare/screenShareSession";
import { shareOf } from "../../types/voice-state";

interface VoiceBarProps {
  channelId: string;
  channelName: string;
}

export const VoiceBar: React.FC<VoiceBarProps> = observer(({ channelId, channelName }) => {
  const {
    voiceParticipants,
    voiceState,
    voiceActiveSpeakerIds,
  } = appStore;
  const voiceIsMuted = voiceState.kind === 'joined' ? voiceState.micMuted : false;
  const share = shareOf(voiceState);
  const shareActive = share.kind === 'active';
  // Anything non-idle/non-picking means the button should become a "stop"
  // affordance — covers in-flight `starting` (recovery from wedged publish)
  // and `failed` (lets the user clear the error state by stopping).
  const shareInFlight = share.kind !== 'idle';
  const { data: groupsWithChannels } = useUserGroupsWithChannels();
  const navigate = useNavigate();

  const groupId = groupsWithChannels?.find((g) =>
    g.channels.some((c) => c.id === channelId)
  )?.id ?? null;

  const toggleMute = () => voiceSession.toggleMute();
  const leave = () => voiceSession.leave();

  // The voice bar is feedback about *other* speakers, so always exclude self.
  // The local participant is flagged `isLocal` by VoiceSessionManager (which
  // owns the device-suffixed identity), so we read it off the list rather than
  // reconstructing `voice-${userId}`.
  const localIdentity = voiceParticipants.find((p) => p.isLocal)?.identity ?? null;
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
        accent={voiceIsMuted ? "#ff6b6b" : "var(--c-accent)"}
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
        accent={shareInFlight ? "#ff6b6b" : "var(--c-accent)"}
        onClick={() => {
          if (shareActive) {
            screenShareSession
              .stop()
              .catch((e) => console.error("[screenshare] stop", e));
            return;
          }
          if (share.kind === "picking") {
            // Button doubles as a cancel affordance while the picker is open.
            screenShareSession
              .cancelPicker()
              .catch((e) => console.warn("[screenshare] cancel:", e))
              .finally(() => {
                appStore.shareCancelPicker();
              });
            return;
          }
          // Any other non-idle share state (e.g. 'starting' that wedged
          // because publishTrack hung on a dead Wayland-portal track, or
          // 'failed') — let the button recover by force-stopping.
          // shareStopped() is safe from any joined-state.
          if (shareInFlight) {
            screenShareSession
              .stop()
              .catch((e) => console.warn("[screenshare] force-stop:", e));
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
              appStore.shareStartPicking(list);
            } catch (e) {
              console.error("[screenshare] enumerate:", e);
              appStore.shareFailed(friendlyScreenShareError(String(e)));
            }
          })();
        }}
        title={
          shareActive
            ? "Stop screen share"
            : share.kind === "picking"
              ? "Cancel"
              : shareInFlight
                ? "Cancel (recover)"
                : "Go live (share screen)"
        }
        aria-label={
          shareActive
            ? "Stop screen share"
            : share.kind === "picking"
              ? "Cancel screen share picker"
              : "Share screen"
        }
        square
      >
        {shareActive ? <MonitorOff size={12} /> : <Monitor size={12} />}
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
            {disambiguateVoiceNames(voiceParticipants).get(lastRemoteSpeakerId)
              ?? voiceParticipants.find(p => p.identity === lastRemoteSpeakerId)?.name}
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
});
