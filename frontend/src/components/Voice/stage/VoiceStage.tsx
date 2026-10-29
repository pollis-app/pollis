// Voice channel content: picking (source picker), preview (roster + Join), spotlight (focused video + filmstrip), or grid. Reads appStore directly.

import React, { useState } from "react";
import { observer } from "mobx-react-lite";
import { useTranslation } from "react-i18next";
import { ArrowLeft, Volume2, VolumeX, Mic, MicOff, Monitor, MonitorOff, Video, VideoOff, LogOut, Phone, PhoneOff, SlidersHorizontal } from "lucide-react";

import { appStore } from "../../../stores/appStore";
import type { VoiceParticipant } from "../../../types";
import { shareOf, cameraOf, screenshareOf, gateOf, micIndicatorOf } from "../../../types/voice-state";
import { useShortcutLabel } from "../../../keyboard";
import {
  micControlTitleKey,
  micControlAriaLabelKey,
  deafenControlTitleKey,
  deafenControlAriaLabelKey,
} from "../voiceControlLabels";
import { isMuted } from "../../../voice/participantAudio";
import { LOCAL_PREVIEW_KEY } from "../../../screenshare/screenShareSession";
import { toggleScreenShare } from "../../../screenshare/screenShareActions";
import { LOCAL_CAMERA_PREVIEW_KEY } from "../../../camera/cameraSession";
import { toggleCamera } from "../../../camera/cameraActions";
import { disambiguateVoiceNames } from "../../../voice/disambiguateNames";
import { userIdFromVoiceIdentity, voiceUserKey } from "../../../voice/identity";
import { voiceSession } from "../../../voice";
import { Button } from "../../ui/Button";
import { NavigableGrid } from "../../ui/NavigableGrid";
import { ScreenSharePicker } from "../ScreenSharePicker";
import { CameraPicker } from "../CameraPicker";
import { StageTile, type StageTileModel } from "./StageTile";
import "./voice-stage.css";

interface VoiceStageProps {
  channelName: string;
  isInCall: boolean;
  onJoin: () => void;
  onLeave: () => void;
  onBack: () => void;
  onOpenSettings: () => void;
  observerParticipants: VoiceParticipant[];
  headerActions?: React.ReactNode;
  footer?: React.ReactNode;
  /** 1:1 DM call: auto-joins and rings, no pre-join roster; leave becomes a hang-up reachable while connecting. */
  callMode?: boolean;
}

export const VoiceStage: React.FC<VoiceStageProps> = observer(
  ({
    channelName,
    isInCall,
    onJoin,
    onLeave,
    onBack,
    onOpenSettings,
    observerParticipants,
    headerActions,
    footer,
    callMode = false,
  }) => {
    const { t } = useTranslation("voice");
    const {
      voiceParticipants,
      voiceActiveSpeakerIds,
      voiceState,
      cameraRemotes,
      setViewingScreenShareTrackKey,
    } = appStore;

    const share = shareOf(voiceState);
    const shareActive = share.kind === "active";
    const sharePicking = share.kind === "picking";
    const shareInFlight = share.kind !== "idle";
    const shareLocalDims = shareActive ? share.dimensions : null;

    const camera = cameraOf(voiceState);
    const cameraActive = camera.kind === "active";
    const cameraPicking = camera.kind === "picking";
    const cameraInFlight = camera.kind !== "idle";
    const cameraLocalDims = cameraActive ? camera.dimensions : null;

    const isJoining = voiceState.kind === "joining";
    const micMuted = voiceState.kind === "joined" ? voiceState.micMuted : false;
    const gate = gateOf(voiceState);
    // Four states (`micIndicatorOf`): PTT-idle and deafened close the mic too.
    const micIndicator = micIndicatorOf(gate);
    const pttCombo = useShortcutLabel("voice.pushToTalk");
    // Listen-only: joined with no capture device.
    const micAvailable = voiceState.kind === "joined" ? voiceState.micAvailable : true;

    const [focusId, setFocusId] = useState<string | null>(null);

    // Drop any fullscreen stream on leave (the viewer lives in AppShell).
    React.useEffect(() => {
      return () => setViewingScreenShareTrackKey(null);
    }, [setViewingScreenShareTrackKey]);

    // One tile per user across devices; prefer local, then the sharing device.
    const mergedParticipants: VoiceParticipant[] = (() => {
      const byUser = new Map<string, VoiceParticipant>();
      const shares = (p: VoiceParticipant): boolean => p.video.kind === "screenshare";
      for (const p of voiceParticipants) {
        const key = userIdFromVoiceIdentity(p.identity);
        const prev = byUser.get(key);
        if (!prev) {
          byUser.set(key, p);
          continue;
        }
        const keep = prev.isLocal
          ? prev
          : p.isLocal
            ? p
            : shares(prev)
              ? prev
              : shares(p)
                ? p
                : prev;
        byUser.set(key, keep);
      }
      return Array.from(byUser.values());
    })();

    const speakingUsers = new Set(voiceActiveSpeakerIds.map(userIdFromVoiceIdentity));

    const displayNames = disambiguateVoiceNames(mergedParticipants);

    // Up to two tiles per participant, camera then screenshare (#394).
    const tilesFor = (p: VoiceParticipant): StageTileModel[] => {
      let screen: { trackKey: string; width?: number; height?: number } | null = null;
      let cam: { trackKey: string; width?: number; height?: number } | null = null;
      if (p.isLocal) {
        if (shareActive) {
          screen = { trackKey: LOCAL_PREVIEW_KEY, width: shareLocalDims?.width, height: shareLocalDims?.height };
        }
        if (cameraActive) {
          cam = { trackKey: LOCAL_CAMERA_PREVIEW_KEY, width: cameraLocalDims?.width, height: cameraLocalDims?.height };
        }
      } else {
        const remote = screenshareOf(p.video);
        if (remote) {
          screen = { trackKey: remote.trackKey, width: remote.width, height: remote.height };
        }
        const c = cameraRemotes[p.identity] ?? cameraRemotes[voiceUserKey(p.identity)];
        if (c) {
          cam = { trackKey: c.trackKey, width: c.width, height: c.height };
        }
      }

      const base = {
        identity: p.identity,
        name: displayNames.get(p.identity) ?? p.name,
        avatarKey: p.avatarKey ?? null,
        isMuted: isMuted(p.audio),
        isLocal: p.isLocal,
        isSpeaking: speakingUsers.has(userIdFromVoiceIdentity(p.identity)) && !isMuted(p.audio),
        connectionQuality: p.connectionQuality,
        isConnecting: p.isLocal && isJoining,
      };

      const tiles: StageTileModel[] = [];
      if (cam) {
        tiles.push({ ...base, tileKey: `${p.identity}:cam`, media: { kind: "camera", ...cam } });
      }
      if (screen) {
        tiles.push({ ...base, tileKey: `${p.identity}:screen`, media: { kind: "screenshare", ...screen } });
      }
      // No video: one audio tile keyed by the bare identity (testid `voice-tile-<identity>`).
      if (tiles.length === 0) {
        tiles.push({ ...base, tileKey: p.identity, media: { kind: "audio" } });
      }
      return tiles;
    };

    const people = mergedParticipants.flatMap(tilesFor);
    // Camera and screenshare are treated alike (#394).
    const streamers = people.filter(
      (p) => p.media.kind === "screenshare" || p.media.kind === "camera",
    );
    // A call is always in its room, so no pre-join preview.
    const stageLive = isInCall || callMode;
    const previewState = !stageLive;
    const spotlight = stageLive && streamers.length > 0;
    // An explicit pick wins, else a screenshare, else the first video.
    const focused =
      streamers.find((p) => p.tileKey === focusId) ??
      streamers.find((p) => p.media.kind === "screenshare") ??
      streamers[0] ??
      null;

    const hasRemoteParticipant = mergedParticipants.some((p) => !p.isLocal);
    const ringStatus =
      callMode && !spotlight && !hasRemoteParticipant
        ? isJoining
          ? t("stage.connecting")
          : t("stage.ringing")
        : null;

    const previewPeople: StageTileModel[] = observerParticipants.map((p) => ({
      tileKey: p.identity,
      identity: p.identity,
      name: p.name,
      avatarKey: p.avatarKey ?? null,
      isMuted: false,
      isLocal: false,
      isSpeaking: false,
      media: { kind: "audio" },
    }));

    const onView = (trackKey: string) => setViewingScreenShareTrackKey(trackKey);

    // Shared by the tray and the connecting footer, so a call can always end.
    const leaveButton = (
      <button
        className="vs-tray-leave"
        data-testid={callMode ? "call-hang-up" : "voice-tray-leave"}
        title={callMode ? t("stage.hangUp") : t("stage.leaveTitle")}
        aria-label={callMode ? t("stage.hangUp") : t("controls.leaveChannel")}
        onClick={onLeave}
      >
        {callMode ? (
          <PhoneOff size={14} />
        ) : (
          // Mirrored exit arrow; `rtl-unmirror` restores it under RTL.
          <LogOut size={14} className="rtl-unmirror" />
        )}
        {callMode ? t("stage.hangUp") : t("stage.leave")}
      </button>
    );

    return (
      <div className="vs-stage font-mono text-xs">
        <div
          className="flex items-center px-4 flex-shrink-0 h-bar border-b border-line text-muted"
        >
          <button
            onClick={onBack}
            aria-label={t("common:actions.back")}
            className="me-3 inline-flex items-center gap-1 leading-none transition-colors text-muted hover:text-accent"
          >
            <ArrowLeft size={12} className="rtl-mirror" />
          </button>
          <span style={{ flex: 1 }} className="flex items-center gap-1.5 text-accent">
            {callMode ? <Phone size={12} /> : <Volume2 size={12} />}
            {channelName}
          </span>
          {headerActions && <div className="flex items-center gap-2">{headerActions}</div>}
        </div>

        <div className="vs-body">
          {sharePicking ? (
            <ScreenSharePicker />
          ) : cameraPicking ? (
            <CameraPicker />
          ) : previewState ? (
            <div className="vs-preview" data-testid="voice-channel-observers">
              {previewPeople.length === 0 ? (
                <div className="vs-preview-empty">
                  <span className="vs-preview-empty-label">{t("stage.emptyRoster")}</span>
                  <Button data-testid="voice-join-cta" variant="primary" onClick={onJoin}>
                    {t("stage.joinVoice")}
                  </Button>
                </div>
              ) : (
                <>
                  <NavigableGrid<StageTileModel>
                    items={previewPeople}
                    getKey={(p) => p.tileKey}
                    autoFocus={false}
                    minCellWidth={180}
                    maxCellWidth={240}
                    renderCell={(p, { focused: f }) => (
                      <StageTile participant={p} mode="preview" focused={f} onView={onView} />
                    )}
                  />
                  <div className="vs-preview-cta">
                    <Button data-testid="voice-join-cta" variant="primary" onClick={onJoin}>
                      {t("stage.joinVoice")}
                    </Button>
                  </div>
                </>
              )}
            </div>
          ) : spotlight ? (
            <div className="vs-spotlight" data-testid="voice-channel-view">
              <div className="vs-spot-main">
                {focused ? (
                  <StageTile participant={focused} mode="big" onFocus={setFocusId} onView={onView} />
                ) : (
                  <div className="vs-tile vs-empty">{t("stage.noActiveStream")}</div>
                )}
              </div>
              <div className="vs-film">
                {people
                  .filter((p) => !focused || p.tileKey !== focused.tileKey)
                  .map((p) => (
                    <div key={p.tileKey} className="vs-film-cell">
                      <StageTile participant={p} mode="film" onFocus={setFocusId} onView={onView} />
                    </div>
                  ))}
              </div>
            </div>
          ) : (
            <>
              {ringStatus && (
                <div className="vs-call-status" data-testid="call-status">
                  {ringStatus}
                </div>
              )}
              <NavigableGrid<StageTileModel>
                items={people}
                getKey={(p) => p.tileKey}
                testId="voice-channel-view"
                emptyLabel={callMode ? t("stage.calling") : t("stage.connecting")}
                autoFocus={false}
                minCellWidth={180}
                maxCellWidth={240}
                renderCell={(p, { focused: f }) => (
                  <StageTile participant={p} mode="grid" focused={f} onView={onView} />
                )}
              />
            </>
          )}
        </div>

        {/* Default tray only once joined; a `footer` override always renders. */}
        {footer ? (
          footer
        ) : voiceState.kind === "joined" ? (
          <div className="vs-foot">
            <div className="vs-foot-side">
              {isInCall || callMode ? (
                <div className="vs-tray">
                  {micAvailable ? (
                    <button
                      // `armed` (PTT idle) is not the red `on` state.
                      className={
                        "vs-tray-btn danger" +
                        (micIndicator === "muted" || micIndicator === "deafened" ? " on" : "") +
                        (micIndicator === "ptt-idle" ? " armed" : "")
                      }
                      data-testid="voice-tray-mute"
                      data-mic-state={micIndicator}
                      title={t(micControlTitleKey(micIndicator), { combo: pttCombo })}
                      aria-label={t(micControlAriaLabelKey(micIndicator), { combo: pttCombo })}
                      onClick={() => voiceSession.toggleMute()}
                    >
                      {micIndicator === "live" || micIndicator === "ptt-idle" ? (
                        <Mic size={15} />
                      ) : (
                        <MicOff size={15} />
                      )}
                    </button>
                  ) : (
                    <button
                      className="vs-tray-btn on"
                      data-testid="voice-tray-listen-only"
                      title={t("controls.listenOnly")}
                      aria-label={t("controls.listenOnly")}
                      disabled
                    >
                      <MicOff size={15} />
                    </button>
                  )}
                  {/* Deafen, shown even listen-only. */}
                  <button
                    className={"vs-tray-btn danger" + (gate.deafened ? " on" : "")}
                    data-testid="voice-tray-deafen"
                    data-deafened={gate.deafened}
                    title={t(deafenControlTitleKey(gate.deafened))}
                    aria-label={t(deafenControlAriaLabelKey(gate.deafened))}
                    onClick={() => voiceSession.toggleDeafen()}
                  >
                    {gate.deafened ? <VolumeX size={15} /> : <Volume2 size={15} />}
                  </button>
                  <button
                    className={"vs-tray-btn" + (shareActive ? " on" : "")}
                    data-testid="voice-tray-screenshare"
                    title={shareActive ? t("controls.stopScreenShare") : shareInFlight ? t("controls.cancelRecover") : t("controls.shareScreen")}
                    aria-label={shareActive ? t("controls.stopScreenShare") : t("controls.shareScreen")}
                    onClick={() => toggleScreenShare(share)}
                  >
                    {shareActive ? <MonitorOff size={15} /> : <Monitor size={15} />}
                  </button>
                  <button
                    className={"vs-tray-btn" + (cameraActive ? " on" : "")}
                    data-testid="voice-tray-camera"
                    title={cameraActive ? t("controls.turnOffCamera") : cameraPicking ? t("common:actions.cancel") : cameraInFlight ? t("controls.cancelRecover") : t("controls.turnOnCamera")}
                    aria-label={cameraActive ? t("controls.turnOffCamera") : t("controls.turnOnCamera")}
                    onClick={() => toggleCamera(camera)}
                  >
                    {cameraActive ? <VideoOff size={15} /> : <Video size={15} />}
                  </button>
                  <div className="vs-tray-sep" />
                  {leaveButton}
                </div>
              ) : (
                <Button data-testid="voice-join-leave-button" variant="primary" size="sm" className="h-[1.75rem]" onClick={onJoin}>
                  <Phone size={14} /> {t("stage.join")}
                </Button>
              )}
            </div>
            <div className="vs-foot-side right">
              <button
                className="flex items-center gap-2 text-xs transition-colors text-muted hover:text-fg"
                data-testid="voice-settings-link"
                onClick={onOpenSettings}
              >
                <SlidersHorizontal size={15} /> {t("stage.settingsLink")}
              </button>
            </div>
          </div>
        ) : callMode ? (
          // Connecting: only hang-up is reachable.
          <div className="vs-foot">
            <div className="vs-foot-side">
              <div className="vs-tray">{leaveButton}</div>
            </div>
          </div>
        ) : null}
      </div>
    );
  },
);
