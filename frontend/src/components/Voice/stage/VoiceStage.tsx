// VoiceStage — the main content area for a voice channel. Flat amber-POSIX
// terminal aesthetic (minimal/informational motion only, no gradients, no
// glow), ported from the design handoff and wired onto the app's live
// voice state.
//
// Body states:
//   - picking   (choosing a screenshare source): the in-app source picker
//                takes over the body (no modal — CLAUDE.md rule).
//   - preview   (not joined): a "who's here" roster (NavigableGrid, same
//                sizing as before) with persistent per-user volume + a
//                solid Join Voice CTA.
//   - spotlight (joined, someone sharing video): the focused video — camera or
//                screenshare — fills the body with a clickable filmstrip below.
//   - grid      (joined, no stream): a reflowing equal grid of tiles.
//
// Reads live data straight off appStore (MobX observer) rather than being a
// pure controlled component — matches the rest of the voice UI. The only
// internal state is which streamer is spotlit.

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
  /** Roster shown in the not-joined preview state. */
  observerParticipants: VoiceParticipant[];
  /** Admin affordances (rename/delete) rendered in the header, right side. */
  headerActions?: React.ReactNode;
  /** When set, replaces the standard footer (e.g. the pending-delete bar). */
  footer?: React.ReactNode;
  /** 1:1 DM call surface. A call auto-joins and rings — it has no pre-join
   *  roster, no "Join Voice" CTA, and no group affordances. The tray's leave
   *  action becomes a hang-up, and a hang-up stays reachable while the call is
   *  still connecting/ringing (before the LiveKit join resolves). Everything
   *  else — camera picker, self-preview, remote camera/screen tiles, per-user
   *  volume, meters — is identical to a group voice channel. */
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
    // Four states, not a bool — see `micIndicatorOf`. Push-to-talk-idle and
    // deafened both close the mic without the user having muted themselves.
    const micIndicator = micIndicatorOf(gate);
    const pttCombo = useShortcutLabel("voice.pushToTalk");
    // Listen-only: joined without a working capture device. The mute button
    // becomes a non-interactive "listening only" indicator.
    const micAvailable = voiceState.kind === "joined" ? voiceState.micAvailable : true;

    const [focusId, setFocusId] = useState<string | null>(null);

    // Drop any fullscreen stream when leaving this view (the viewer is a
    // global overlay in AppShell and would otherwise stay pinned).
    React.useEffect(() => {
      return () => setViewingScreenShareTrackKey(null);
    }, [setViewingScreenShareTrackKey]);

    // Merge a user's multiple device-identities (voice-{user}:{deviceA},
    // voice-{user}:{deviceB}) into one tile — Discord-style, one entry per
    // user, not per device. Without this a multi-device user (or a brief
    // reconnect overlap) renders as "ants" + "ants (1)". Prefer the local
    // device, then whichever device is screensharing, then the first seen.
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

    // Speaking is user-scoped so a merged tile lights up when ANY of the
    // user's devices is the active speaker.
    const speakingUsers = new Set(voiceActiveSpeakerIds.map(userIdFromVoiceIdentity));

    const displayNames = disambiguateVoiceNames(mergedParticipants);

    // Expand a live VoiceParticipant into its stage tiles. A participant can
    // publish a screenshare AND a webcam at the same time (#394), so this
    // returns UP TO TWO tiles — one per active video source — rather than one
    // tile that has to pick a single surface. Ordering (camera first, then
    // screenshare) keeps a person's face next to their screen in the grid.
    //   none   → [audio]
    //   camera → [camera]
    //   screen → [screenshare]
    //   both   → [camera, screenshare]
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
      // No video → a single audio/avatar tile keyed by the bare identity (so a
      // plain voice participant's testid stays `voice-tile-<identity>`).
      if (tiles.length === 0) {
        tiles.push({ ...base, tileKey: p.identity, media: { kind: "audio" } });
      }
      return tiles;
    };

    const people = mergedParticipants.flatMap(tilesFor);
    // Any video tile — camera OR screenshare — can be spotlit and shows in the
    // spotlight/filmstrip. The two are treated identically at the stage level
    // (#394); the container doesn't care where the pixels come from.
    const streamers = people.filter(
      (p) => p.media.kind === "screenshare" || p.media.kind === "camera",
    );
    // A call is always "in" its room (it auto-joins and rings), so it never
    // shows the group pre-join preview and its stage goes live immediately.
    const stageLive = isInCall || callMode;
    const previewState = !stageLive;
    const spotlight = stageLive && streamers.length > 0;
    // Default the big view to a screenshare when one exists (the natural focal
    // point of "watch my screen"), otherwise the first video — but an explicit
    // user pick (focusId) always wins.
    const focused =
      streamers.find((p) => p.tileKey === focusId) ??
      streamers.find((p) => p.media.kind === "screenshare") ??
      streamers[0] ??
      null;

    // Ringing/connecting caption — call-only, shown above the (self-only) grid
    // until the counterparty joins. Replaces the group "Join Voice" preview.
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

    // Leave affordance — a plain "Leave" in a channel, a red "Hang up" in a
    // call. Shared between the full in-call tray and the connecting-state
    // footer so a call can always be ended.
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
          // Exit arrow points against the reading direction (mirrored) — a
          // "leave" gesture. `rtl-unmirror` returns it to its natural drawing
          // under `dir=rtl`, where "backwards" is rightwards.
          <LogOut size={14} className="rtl-unmirror" />
        )}
        {callMode ? t("stage.hangUp") : t("stage.leave")}
      </button>
    );

    return (
      <div className="vs-stage font-mono text-xs">
        {/* ---------- header (compact, matches the rest of the app) ---------- */}
        <div
          className="flex items-center px-4 flex-shrink-0 h-[var(--bar-h)]"
          style={{ borderBottom: "1px solid var(--c-border)", color: "var(--c-text-muted)" }}
        >
          <button
            onClick={onBack}
            aria-label={t("common:actions.back")}
            className="me-3 inline-flex items-center gap-1 leading-none transition-colors text-[var(--c-text-muted)] hover:text-[var(--c-accent)]"
          >
            <ArrowLeft size={12} className="rtl-mirror" />
          </button>
          <span style={{ flex: 1, color: "var(--c-accent)" }} className="flex items-center gap-1.5">
            {callMode ? <Phone size={12} /> : <Volume2 size={12} />}
            {channelName}
          </span>
          {headerActions && <div className="flex items-center gap-2">{headerActions}</div>}
        </div>

        {/* ---------- body ---------- */}
        <div className="vs-body">
          {sharePicking ? (
            <ScreenSharePicker />
          ) : cameraPicking ? (
            <CameraPicker />
          ) : previewState ? (
            <div className="vs-preview" data-testid="voice-channel-observers">
              {previewPeople.length === 0 ? (
                // Empty: label + CTA centered together as one group.
                <div className="vs-preview-empty">
                  <span className="vs-preview-empty-label">{t("stage.emptyRoster")}</span>
                  <Button data-testid="voice-join-cta" variant="primary" onClick={onJoin}>
                    {t("stage.joinVoice")}
                  </Button>
                </div>
              ) : (
                // Populated: roster, then the CTA pinned below the users.
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

        {/* ---------- footer (fixed height) ----------
            Only the default call-tray footer is gated on a completed join
            (matching the global VoiceBar, which mounts on
            voiceState.kind === 'joined', not during 'joining'). The
            not-joined preview keeps its own "Join Voice" CTA in the body, so
            hiding this bar until connected strands nothing. A `footer`
            override (e.g. the admin pending-delete bar) always renders. */}
        {footer ? (
          footer
        ) : voiceState.kind === "joined" ? (
          <div className="vs-foot">
            <div className="vs-foot-side">
              {isInCall || callMode ? (
                <div className="vs-tray">
                  {micAvailable ? (
                    <button
                      // `armed` (push-to-talk idle) is deliberately NOT the
                      // red `on` state: nothing is wrong, the mic is simply
                      // waiting for the key.
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
                  {/* Deafen — next to mute, as on Discord. Rendered whether
                      or not a mic exists: a listen-only session still has
                      incoming audio worth silencing. */}
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
                className="flex items-center gap-2 text-xs transition-colors text-[var(--c-text-muted)] hover:text-[var(--c-text)]"
                data-testid="voice-settings-link"
                onClick={onOpenSettings}
              >
                <SlidersHorizontal size={15} /> {t("stage.settingsLink")}
              </button>
            </div>
          </div>
        ) : callMode ? (
          // Connecting/ringing: the mic/share/camera controls need a completed
          // join, but a hang-up must stay reachable the whole time.
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
