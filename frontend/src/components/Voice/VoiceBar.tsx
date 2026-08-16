import React from "react";
import { useNavigate } from "@tanstack/react-router";
import { useTranslation } from "react-i18next";
import { observer } from "mobx-react-lite";
import { appStore } from "../../stores/appStore";
import { useUserGroupsWithChannels } from "../../hooks/queries/useGroups";
import { Volume2, VolumeX, Mic, MicOff, PhoneOff, SlidersHorizontal, Monitor, MonitorOff, Video, VideoOff } from "lucide-react";
import { PillButton } from "../ui/PillButton";
import { voiceSession } from "../../voice";
import { disambiguateVoiceNames } from "../../voice/disambiguateNames";
import { userIdFromVoiceIdentity } from "../../voice/identity";
import { toggleScreenShare } from "../../screenshare/screenShareActions";
import { toggleCamera } from "../../camera/cameraActions";
import { shareOf, cameraOf, gateOf, micIndicatorOf } from "../../types/voice-state";
import { useShortcutLabel } from "../../keyboard";
import {
  micControlTitleKey,
  micControlAriaLabelKey,
  deafenControlTitleKey,
  deafenControlAriaLabelKey,
} from "./voiceControlLabels";

interface VoiceBarProps {
  channelId: string;
  channelName: string;
}

export const VoiceBar: React.FC<VoiceBarProps> = observer(({ channelId, channelName }) => {
  const { t } = useTranslation("voice");
  const {
    voiceParticipants,
    voiceState,
    voiceActiveSpeakerIds,
  } = appStore;
  const gate = gateOf(voiceState);
  // Four states, not a bool: deafened and push-to-talk-idle both stop the
  // mic without the user having muted themselves, and showing either as a
  // plain mute is what makes push-to-talk feel broken.
  const micIndicator = micIndicatorOf(gate);
  const pttCombo = useShortcutLabel("voice.pushToTalk");
  // Listen-only: joined without a working capture device. The mute toggle
  // becomes a non-interactive "listening only" indicator.
  const micAvailable = voiceState.kind === 'joined' ? voiceState.micAvailable : true;
  const share = shareOf(voiceState);
  const shareActive = share.kind === 'active';
  // Anything non-idle/non-picking means the button should become a "stop"
  // affordance — covers in-flight `starting` (recovery from wedged publish)
  // and `failed` (lets the user clear the error state by stopping).
  const shareInFlight = share.kind !== 'idle';
  const camera = cameraOf(voiceState);
  const cameraActive = camera.kind === 'active';
  const cameraInFlight = camera.kind !== 'idle';
  const { data: groupsWithChannels } = useUserGroupsWithChannels();
  const navigate = useNavigate();

  const groupId = groupsWithChannels?.find((g) =>
    g.channels.some((c) => c.id === channelId)
  )?.id ?? null;

  const toggleMute = () => voiceSession.toggleMute();
  const toggleDeafen = () => voiceSession.toggleDeafen();
  const leave = () => voiceSession.leave();

  // The voice bar is feedback about *other* speakers, so always exclude self.
  // The local participant is flagged `isLocal` by VoiceSessionManager (which
  // owns the device-suffixed identity), so we read it off the list rather than
  // reconstructing `voice-${userId}`.
  const localIdentity = voiceParticipants.find((p) => p.isLocal)?.identity ?? null;
  const remoteActiveSpeakerIds = voiceActiveSpeakerIds.filter((id) => id !== localIdentity);
  const lastRemoteSpeakerId = remoteActiveSpeakerIds.at(-1);

  // Count distinct *users*, not raw participant entries: a user joined from two
  // devices (or mid-reconnect overlap) is one person, and voice + screenshare
  // are two publications by the same identity, not two people. Dedupe by user
  // id the same way VoiceStage merges device-identities into one tile, so this
  // matches the sidebar's room count.
  const participantCount = new Set(
    voiceParticipants.map((p) => userIdFromVoiceIdentity(p.identity)),
  ).size;

  return (
    <div
      data-testid="voice-bar"
      className="flex items-center ps-1 pe-3 gap-2 font-mono text-xs flex-shrink-0 border-t border-line bg-surface text-muted"
      style={{
        height: 28,
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
            ? t("bar.returnToCall")
            : t("bar.goToChannel", { name: channelName })
        }
      >
        <Volume2 size={12} />
        {channelName}
      </PillButton>

      {/* Mute toggle — or, listen-only, a static "listening only" indicator.
          `data-mic-state` carries the full four-state indicator so tests and
          the tray agree on what is being shown. Push-to-talk-idle keeps the
          live Mic glyph (the mic is armed, not switched off) but dims it. */}
      {micAvailable ? (
        <PillButton
          data-testid="voice-bar-mute-button"
          data-mic-state={micIndicator}
          accent={
            micIndicator === "live"
              ? "var(--c-accent)"
              : micIndicator === "ptt-idle"
                ? "var(--c-text-dim)"
                : "var(--c-danger)"
          }
          onClick={toggleMute}
          title={t(micControlTitleKey(micIndicator), { combo: pttCombo })}
          aria-label={t(micControlAriaLabelKey(micIndicator), { combo: pttCombo })}
          square
        >
          {micIndicator === "live" || micIndicator === "ptt-idle" ? (
            <Mic size={12} />
          ) : (
            <MicOff size={12} />
          )}
        </PillButton>
      ) : (
        <PillButton
          data-testid="voice-bar-listen-only"
          accent="var(--c-text-dim)"
          title={t("controls.listenOnly")}
          aria-label={t("controls.listenOnly")}
          square
        >
          <MicOff size={12} />
        </PillButton>
      )}

      {/* Deafen toggle. Sits next to mute, as on Discord — deafen implies
          mute, and undeafening restores whatever mute state it displaced.
          Shown even listen-only: with no capture device you can still have
          incoming audio worth silencing. */}
      <PillButton
        data-testid="voice-bar-deafen-button"
        data-deafened={gate.deafened}
        accent={gate.deafened ? "var(--c-danger)" : "var(--c-accent)"}
        onClick={toggleDeafen}
        title={t(deafenControlTitleKey(gate.deafened))}
        aria-label={t(deafenControlAriaLabelKey(gate.deafened))}
        square
      >
        {gate.deafened ? <VolumeX size={12} /> : <Volume2 size={12} />}
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
        accent={shareInFlight ? "var(--c-danger)" : "var(--c-accent)"}
        onClick={() => toggleScreenShare(share)}
        title={
          shareActive
            ? t("controls.stopScreenShare")
            : share.kind === "picking"
              ? t("common:actions.cancel")
              : shareInFlight
                ? t("controls.cancelRecover")
                : t("controls.goLive")
        }
        aria-label={
          shareActive
            ? t("controls.stopScreenShare")
            : share.kind === "picking"
              ? t("controls.cancelSharePicker")
              : t("controls.shareScreen")
        }
        square
      >
        {shareActive ? <MonitorOff size={12} /> : <Monitor size={12} />}
      </PillButton>

      {/* Camera toggle — webcam published into the same voice room as a
          TrackSource::Camera track. With one camera it starts directly; with
          several the in-app picker (CameraPicker) takes over the stage. */}
      <PillButton
        data-testid="voice-bar-camera-button"
        accent={cameraInFlight && !cameraActive ? "var(--c-danger)" : "var(--c-accent)"}
        onClick={() => toggleCamera(camera)}
        title={
          cameraActive
            ? t("controls.turnOffCamera")
            : camera.kind === "picking"
              ? t("common:actions.cancel")
              : cameraInFlight
                ? t("controls.cancelRecover")
                : t("controls.turnOnCamera")
        }
        aria-label={
          cameraActive
            ? t("controls.turnOffCamera")
            : camera.kind === "picking"
              ? t("controls.cancelCameraPicker")
              : t("controls.turnOnCamera")
        }
        square
      >
        {cameraActive ? <VideoOff size={12} /> : <Video size={12} />}
      </PillButton>

      {/* Leave button */}
      <PillButton
        data-testid="voice-bar-leave-button"
        accent="var(--c-danger)"
        onClick={leave}
        title={t("controls.leaveChannel")}
        aria-label={t("controls.leaveChannel")}
        square
      >
        <PhoneOff size={12} />
      </PillButton>

      <span className="text-line">|</span>

      {/* Participant count */}
      <span data-testid="voice-bar-participant-count" className="text-dim">
        {t("bar.participants", { count: participantCount })}
      </span>

      {/* Security indicator — audio is transport-encrypted (TLS) but not E2EE for v1 */}
      <span
        data-testid="voice-bar-security-indicator"
        style={{ marginInlineStart: "auto" }}
        className="flex items-center gap-1 text-dim"
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
        aria-label={t("bar.settings")}
        title={t("bar.settings")}
        className="flex items-center justify-center transition-colors flex-shrink-0 text-muted hover:text-accent border-0"
        style={{
          width: 20,
          height: 20,
          background: "none",
          padding: 0,
          cursor: "pointer",
        }}
      >
        <SlidersHorizontal size={14} />
      </button>
    </div>
  );
});
