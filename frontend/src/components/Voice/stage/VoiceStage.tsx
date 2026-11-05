// Voice channel content: picking (source picker), preview (roster + Join), focus (one stream, chrome hidden), spotlight (focused video + filmstrip), or grid. Reads appStore directly.

import React, { useCallback, useRef, useState } from "react";
import { observer } from "mobx-react-lite";
import { useTranslation } from "react-i18next";
import { ArrowLeft, Volume2, VolumeX, Mic, MicOff, Monitor, MonitorOff, Video, VideoOff, LogOut, Phone, PhoneOff, SlidersHorizontal, LayoutGrid, Focus } from "lucide-react";

import { appStore } from "../../../stores/appStore";
import type { VoiceParticipant } from "../../../types";
import { shareOf, cameraOf, screenshareOf, gateOf, micIndicatorOf, isMediaBusy } from "../../../types/voice-state";
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
import { LoadingSpinner } from "../../ui/LoaderSpinner";
import { NavigableGrid } from "../../ui/NavigableGrid";
import { ScreenSharePicker } from "../ScreenSharePicker";
import { CameraPicker } from "../CameraPicker";
import { ScreenShareViewer } from "../ScreenShareViewer";
import { SpeakingAnnouncer } from "../SpeakingAnnouncer";
import { StageTile, type StageTileModel } from "./StageTile";
import { probeRender } from "../../../utils/renderProbe";
import "./voice-stage.css";

/** True when two models would draw identically, so the previous reference can be reused (`React.memo`). */
function sameTileModel(a: StageTileModel, b: StageTileModel): boolean {
  if (
    a.tileKey !== b.tileKey ||
    a.identity !== b.identity ||
    a.name !== b.name ||
    a.avatarKey !== b.avatarKey ||
    a.isMuted !== b.isMuted ||
    a.isLocal !== b.isLocal ||
    a.isSpeaking !== b.isSpeaking ||
    a.connectionQuality !== b.connectionQuality ||
    a.isConnecting !== b.isConnecting ||
    a.isReconnecting !== b.isReconnecting ||
    a.stale !== b.stale ||
    a.media.kind !== b.media.kind
  ) {
    return false;
  }
  if (a.media.kind === "audio" || b.media.kind === "audio") {
    return true;
  }
  return (
    a.media.trackKey === b.media.trackKey &&
    a.media.width === b.media.width &&
    a.media.height === b.media.height
  );
}

/** Stable key accessor: an inline arrow would defeat the grid's memo on every stage render. */
const tileKeyOf = (p: StageTileModel): string => p.tileKey;

/** Which stream a tile model carries, or null for an audio-only tile. */
function trackKeyOf(p: StageTileModel): string | null {
  return p.media.kind === "audio" ? null : p.media.trackKey;
}

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

type StageLayout = "auto" | "grid";

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
    probeRender("VoiceStage");
    const { t } = useTranslation("voice");
    const {
      voiceParticipants,
      voiceActiveSpeakerIds,
      voiceState,
      cameraRemotes,
      viewingScreenShareTrackKey,
      setViewingScreenShareTrackKey,
    } = appStore;

    const share = shareOf(voiceState);
    const shareActive = share.kind === "active";
    const sharePicking = share.kind === "picking";
    const shareInFlight = share.kind !== "idle";
    const shareBusy = isMediaBusy(share);
    const shareLocalDims = shareActive ? share.dimensions : null;

    const camera = cameraOf(voiceState);
    const cameraActive = camera.kind === "active";
    const cameraPicking = camera.kind === "picking";
    const cameraInFlight = camera.kind !== "idle";
    const cameraBusy = isMediaBusy(camera);
    const cameraLocalDims = cameraActive ? camera.dimensions : null;

    const isJoining = voiceState.kind === "joining";
    const reconnecting = voiceState.kind === "joined" ? voiceState.reconnecting : null;
    const speakerUnavailable = voiceState.kind === "joined" && !voiceState.speakerAvailable;
    const gate = gateOf(voiceState);
    // Four states (`micIndicatorOf`): PTT-idle and deafened close the mic too.
    const micIndicator = micIndicatorOf(gate);
    const pttCombo = useShortcutLabel("voice.pushToTalk");
    // Listen-only: joined with no capture device.
    const micAvailable = voiceState.kind === "joined" ? voiceState.micAvailable : true;

    const [focusId, setFocusId] = useState<string | null>(null);
    // "auto" spotlights whoever streams (Discord's default); "grid" keeps everyone equal while they do.
    const [layout, setLayout] = useState<StageLayout>("auto");
    // Previous render's models by tile key: an unchanged tile keeps its reference, so one
    // participant's speaking flip re-renders exactly that tile (#874 for the stage). The list is
    // kept too, so a render that changes nothing hands the grid the same array.
    const modelCache = useRef<{ byKey: Map<string, StageTileModel>; list: StageTileModel[] }>({
      byKey: new Map(),
      list: [],
    });

    // Drop any focused stream on leave.
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
        isReconnecting: p.isLocal && reconnecting !== null,
        // Remote media is paused until the room is back; dim rather than drop.
        stale: !p.isLocal && reconnecting !== null,
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

    const people = (() => {
      const cache = modelCache.current;
      const byKey = new Map<string, StageTileModel>();
      let unchanged = true;
      const stable = mergedParticipants.flatMap(tilesFor).map((model, i) => {
        const prev = cache.byKey.get(model.tileKey);
        const keep = prev && sameTileModel(prev, model) ? prev : model;
        if (keep !== cache.list[i]) {
          unchanged = false;
        }
        byKey.set(model.tileKey, keep);
        return keep;
      });
      const list = unchanged && stable.length === cache.list.length ? cache.list : stable;
      modelCache.current = { byKey, list };
      return list;
    })();
    // Camera and screenshare are treated alike (#394).
    const streamers = people.filter(
      (p) => p.media.kind === "screenshare" || p.media.kind === "camera",
    );
    // A call is always in its room, so no pre-join preview.
    const stageLive = isInCall || callMode;
    const previewState = !stageLive;
    const spotlight = stageLive && streamers.length > 0 && layout !== "grid";
    // An explicit pick wins, else a screenshare, else the first video.
    const focused =
      streamers.find((p) => p.tileKey === focusId) ??
      streamers.find((p) => p.media.kind === "screenshare") ??
      streamers[0] ??
      null;
    // Focus mode: one stream fills the body, the only video consumer for that track.
    const focusView = stageLive && viewingScreenShareTrackKey
      ? streamers.find((p) => trackKeyOf(p) === viewingScreenShareTrackKey) ?? null
      : null;

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

    const onView = useCallback(
      (trackKey: string) => setViewingScreenShareTrackKey(trackKey),
      [setViewingScreenShareTrackKey],
    );
    const closeFocus = useCallback(() => setViewingScreenShareTrackKey(null), [setViewingScreenShareTrackKey]);
    // Enter on a focused grid cell spotlights a video tile (audio tiles have nothing to spotlight).
    const onActivate = useCallback((p: StageTileModel) => {
      if (p.media.kind !== "audio") {
        setFocusId(p.tileKey);
      }
    }, []);
    const renderPreviewCell = useCallback(
      (p: StageTileModel, { focused: f }: { focused: boolean }) => (
        <StageTile participant={p} mode="preview" focused={f} onView={onView} />
      ),
      [onView],
    );
    const renderGridCell = useCallback(
      (p: StageTileModel, { focused: f }: { focused: boolean }) => (
        <StageTile participant={p} mode="grid" focused={f} onFocus={setFocusId} onView={onView} />
      ),
      [onView],
    );

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
      <div className="vs-stage font-mono text-xs" data-focus={focusView ? "true" : undefined}>
        {/* Focus mode hides the header: the stream gets the whole body, the tray stays. */}
        {!focusView && (
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
        )}

        {stageLive && <SpeakingAnnouncer />}

        <div className="vs-body">
          {reconnecting && (
            <div className="vs-notice" role="status" data-testid="voice-stage-reconnecting">
              <LoadingSpinner size="sm" />
              {reconnecting.attempt > 1
                ? t("stage.reconnectingAttempt", { attempt: reconnecting.attempt })
                : t("stage.reconnecting")}
            </div>
          )}
          {speakerUnavailable && !reconnecting && (
            <div className="vs-notice" role="status" data-testid="voice-stage-speaker-unavailable">
              <VolumeX size={13} />
              {t("stage.speakerUnavailable")}
            </div>
          )}
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
                    getKey={tileKeyOf}
                    autoFocus={false}
                    minCellWidth={180}
                    maxCellWidth={240}
                    renderCell={renderPreviewCell}
                  />
                  <div className="vs-preview-cta">
                    <Button data-testid="voice-join-cta" variant="primary" onClick={onJoin}>
                      {t("stage.joinVoice")}
                    </Button>
                  </div>
                </>
              )}
            </div>
          ) : focusView ? (
            <ScreenShareViewer tile={focusView} onClose={closeFocus} />
          ) : spotlight ? (
            <div className="vs-spotlight" data-testid="voice-channel-view">
              <div className="vs-spot-main">
                {focused ? (
                  // Keyed, so switching the spotlight remounts the WebGL canvas for the new track.
                  <StageTile key={focused.tileKey} participant={focused} mode="big" onFocus={setFocusId} onView={onView} />
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
                getKey={tileKeyOf}
                testId="voice-channel-view"
                emptyLabel={callMode ? t("stage.calling") : t("stage.connecting")}
                autoFocus={false}
                minCellWidth={180}
                maxCellWidth={240}
                onActivate={onActivate}
                renderCell={renderGridCell}
              />
            </>
          )}
        </div>

        {/* The tray mounts with the join (not after it) so the footer never jumps in late, and Leave
            stays reachable while connecting; a `footer` override always renders. */}
        {footer ? (
          footer
        ) : voiceState.kind === "joined" || voiceState.kind === "joining" ? (
          <div className="vs-foot" data-phase={voiceState.kind}>
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
                      aria-pressed={micIndicator === "muted" || micIndicator === "deafened"}
                      disabled={isJoining}
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
                    aria-pressed={gate.deafened}
                    disabled={isJoining}
                    onClick={() => voiceSession.toggleDeafen()}
                  >
                    {gate.deafened ? <VolumeX size={15} /> : <Volume2 size={15} />}
                  </button>
                  <button
                    className={"vs-tray-btn" + (shareActive ? " on" : "")}
                    data-testid="voice-tray-screenshare"
                    data-busy={shareBusy || undefined}
                    title={shareActive ? t("controls.stopScreenShare") : shareInFlight ? t("controls.cancelRecover") : t("controls.shareScreen")}
                    aria-label={shareActive ? t("controls.stopScreenShare") : t("controls.shareScreen")}
                    aria-pressed={shareActive}
                    disabled={isJoining}
                    onClick={() => toggleScreenShare(share)}
                  >
                    {shareBusy ? <LoadingSpinner size="sm" /> : shareActive ? <MonitorOff size={15} /> : <Monitor size={15} />}
                  </button>
                  <button
                    className={"vs-tray-btn" + (cameraActive ? " on" : "")}
                    data-testid="voice-tray-camera"
                    data-busy={cameraBusy || undefined}
                    title={cameraActive ? t("controls.turnOffCamera") : cameraPicking ? t("common:actions.cancel") : cameraInFlight ? t("controls.cancelRecover") : t("controls.turnOnCamera")}
                    aria-label={cameraActive ? t("controls.turnOffCamera") : t("controls.turnOnCamera")}
                    aria-pressed={cameraActive}
                    disabled={isJoining}
                    onClick={() => toggleCamera(camera)}
                  >
                    {cameraBusy ? <LoadingSpinner size="sm" /> : cameraActive ? <VideoOff size={15} /> : <Video size={15} />}
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
              {/* Grid ↔ spotlight only matters while someone streams. */}
              {stageLive && streamers.length > 0 && !focusView && (
                <button
                  className={"vs-tray-btn" + (layout === "grid" ? " on" : "")}
                  data-testid="voice-layout-toggle"
                  data-layout={layout === "grid" ? "grid" : "spotlight"}
                  title={layout === "grid" ? t("stage.layoutSpotlight") : t("stage.layoutGrid")}
                  aria-label={layout === "grid" ? t("stage.layoutSpotlight") : t("stage.layoutGrid")}
                  aria-pressed={layout === "grid"}
                  onClick={() => setLayout(layout === "grid" ? "auto" : "grid")}
                >
                  {layout === "grid" ? <Focus size={15} /> : <LayoutGrid size={15} />}
                </button>
              )}
              <button
                className="flex items-center gap-2 text-xs transition-colors text-muted hover:text-fg"
                data-testid="voice-settings-link"
                onClick={onOpenSettings}
              >
                <SlidersHorizontal size={15} /> {t("stage.settingsLink")}
              </button>
            </div>
          </div>
        ) : null}
      </div>
    );
  },
);
