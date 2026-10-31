import React from "react";
import { useNavigate } from "@tanstack/react-router";
import { useTranslation } from "react-i18next";
import { observer } from "mobx-react-lite";
import { appStore } from "../../stores/appStore";
import { useUserGroupsWithChannels } from "../../hooks/queries/useGroups";
import { Volume2, VolumeX, Mic, MicOff, PhoneOff, SlidersHorizontal, Monitor, MonitorOff, Video, VideoOff, AudioLines } from "lucide-react";
import { PillButton } from "../ui/PillButton";
import { voiceSession } from "../../voice";
import { disambiguateVoiceNames } from "../../voice/disambiguateNames";
import { userIdFromVoiceIdentity } from "../../voice/identity";
import { toggleScreenShare } from "../../screenshare/screenShareActions";
import { toggleCamera } from "../../camera/cameraActions";
import { shareOf, cameraOf, gateOf, micIndicatorOf, activeCallIdOf } from "../../types/voice-state";
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
  // Same "return to call" target as the DM header (#1640).
  const activeCallId = activeCallIdOf(voiceState);
  // Four states: PTT-idle and deafened close the mic too.
  const micIndicator = micIndicatorOf(gate);
  const pttCombo = useShortcutLabel("voice.pushToTalk");
  // Listen-only: joined with no capture device.
  const micAvailable = voiceState.kind === 'joined' ? voiceState.micAvailable : true;
  const share = shareOf(voiceState);
  const shareActive = share.kind === 'active';
  // Any non-idle state is a "stop" (clears `starting` and `failed`).
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

  // Exclude every identity of the local user (other devices, reconnect overlap), not just `isLocal`.
  const localIdentity = voiceParticipants.find((p) => p.isLocal)?.identity ?? null;
  const localUserId = localIdentity ? userIdFromVoiceIdentity(localIdentity) : null;
  const remoteActiveSpeakerIds = voiceActiveSpeakerIds.filter(
    (id) => userIdFromVoiceIdentity(id) !== localUserId,
  );
  const lastRemoteSpeakerId = remoteActiveSpeakerIds.at(-1);

  // Distinct users, as VoiceStage merges tiles.
  const participantCount = new Set(
    voiceParticipants.map((p) => userIdFromVoiceIdentity(p.identity)),
  ).size;

  return (
    <div
      data-testid="voice-bar"
      className="flex items-center ps-1 pe-3 gap-2 font-mono text-xs flex-shrink-0 min-h-composer-flush border-t border-line bg-surface text-muted"
    >
      <PillButton
        data-testid="voice-bar-channel-name"
        accent="var(--c-accent)"
        onClick={() => {
          if (activeCallId) {
            navigate({ to: "/call/$callId", params: { callId: activeCallId } });
          } else if (groupId) {
            navigate({ to: "/groups/$groupId/voice/$channelId", params: { groupId, channelId } });
          }
        }}
        title={
          activeCallId
            ? t("bar.returnToCall")
            : t("bar.goToChannel", { name: channelName })
        }
      >
        <Volume2 size={12} />
        {channelName}
      </PillButton>

      {/* `data-mic-state` carries the four-state indicator. */}
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

      {/* Deafen, shown even listen-only. */}
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

      {/* macOS uses the in-app picker (SCContentSharingPicker crashes, #283); an empty source list means a system picker. */}
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

      {/* Indicator only: audio can't change mid-share. */}
      {share.kind === "active" && share.audio.kind !== "off" ? (
        <span
          data-testid="voice-bar-share-audio-indicator"
          className={`flex items-center px-1 ${
            share.audio.kind === "live"
              ? "text-accent"
              : share.audio.kind === "unavailable"
                ? "text-danger"
                : "text-muted"
          }`}
          title={
            share.audio.kind === "unavailable"
              ? share.audio.reason
              : share.audio.kind === "live"
                ? t("share.audioLive")
                : t("share.audioPending")
          }
          aria-label={
            share.audio.kind === "unavailable"
              ? t("share.audioOffLabel")
              : share.audio.kind === "live"
                ? t("share.audioLive")
                : t("share.audioPending")
          }
        >
          {share.audio.kind === "unavailable" ? (
            <VolumeX size={12} />
          ) : (
            <AudioLines size={12} />
          )}
        </span>
      ) : null}

      {/* Camera: starts directly with one, else CameraPicker. */}
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

      <span data-testid="voice-bar-participant-count" className="text-dim">
        {t("bar.participants", { count: participantCount })}
      </span>

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
