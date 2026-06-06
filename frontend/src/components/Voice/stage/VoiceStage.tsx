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
//   - spotlight (joined, someone streaming): the focused screenshare fills
//                the body with a clickable filmstrip below.
//   - grid      (joined, no stream): a reflowing equal grid of tiles.
//
// Reads live data straight off appStore (MobX observer) rather than being a
// pure controlled component — matches the rest of the voice UI. The only
// internal state is which streamer is spotlit.

import React, { useState } from "react";
import { observer } from "mobx-react-lite";
import { ArrowLeft, Volume2, Mic, MicOff, Monitor, MonitorOff, LogOut, Phone, SlidersHorizontal } from "lucide-react";

import { appStore } from "../../../stores/appStore";
import type { VoiceParticipant } from "../../../types";
import { shareOf } from "../../../types/voice-state";
import { LOCAL_PREVIEW_KEY } from "../../../screenshare/screenShareSession";
import { toggleScreenShare } from "../../../screenshare/screenShareActions";
import { disambiguateVoiceNames } from "../../../voice/disambiguateNames";
import { voiceUserKey } from "../../../voice/identity";
import { voiceSession } from "../../../voice";
import { Button } from "../../ui/Button";
import { NavigableGrid } from "../../ui/NavigableGrid";
import { ScreenSharePicker } from "../ScreenSharePicker";
import { StageTile, type StageParticipant } from "./StageTile";
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
  }) => {
    const {
      voiceParticipants,
      voiceActiveSpeakerIds,
      voiceState,
      screenShareRemotes,
      setViewingScreenShareTrackKey,
    } = appStore;

    const share = shareOf(voiceState);
    const shareActive = share.kind === "active";
    const sharePicking = share.kind === "picking";
    const shareInFlight = share.kind !== "idle";
    const shareLocalDims = shareActive ? share.dimensions : null;
    const isJoining = voiceState.kind === "joining";
    const micMuted = voiceState.kind === "joined" ? voiceState.micMuted : false;

    const [focusId, setFocusId] = useState<string | null>(null);

    // Drop any fullscreen stream when leaving this view (the viewer is a
    // global overlay in AppShell and would otherwise stay pinned).
    React.useEffect(() => {
      return () => setViewingScreenShareTrackKey(null);
    }, [setViewingScreenShareTrackKey]);

    const displayNames = disambiguateVoiceNames(voiceParticipants);

    // Map a live VoiceParticipant onto the tile's view model, resolving
    // whichever screenshare track (local preview or remote) it's publishing.
    const resolve = (p: VoiceParticipant): StageParticipant => {
      let streamTrackKey: string | undefined;
      let streamWidth: number | undefined;
      let streamHeight: number | undefined;
      if (p.isLocal && shareActive) {
        streamTrackKey = LOCAL_PREVIEW_KEY;
        streamWidth = shareLocalDims?.width;
        streamHeight = shareLocalDims?.height;
      } else if (!p.isLocal) {
        const remote =
          screenShareRemotes[p.identity] ??
          screenShareRemotes[voiceUserKey(p.identity)];
        if (remote) {
          streamTrackKey = remote.trackKey;
          streamWidth = remote.width;
          streamHeight = remote.height;
        }
      }
      return {
        identity: p.identity,
        name: displayNames.get(p.identity) ?? p.name,
        avatarKey: p.avatarKey ?? null,
        isMuted: p.isMuted,
        isLocal: p.isLocal,
        isSpeaking: voiceActiveSpeakerIds.includes(p.identity) && !p.isMuted,
        connectionQuality: p.connectionQuality,
        streamTrackKey,
        streamWidth,
        streamHeight,
        isConnecting: p.isLocal && isJoining,
      };
    };

    const people = voiceParticipants.map(resolve);
    const streamers = people.filter((p) => p.streamTrackKey !== undefined);
    const spotlight = isInCall && streamers.length > 0;
    const focused =
      streamers.find((p) => p.identity === focusId) ?? streamers[0] ?? null;

    const previewPeople: StageParticipant[] = observerParticipants.map((p) => ({
      identity: p.identity,
      name: p.name,
      avatarKey: p.avatarKey ?? null,
      isMuted: false,
      isLocal: false,
      isSpeaking: false,
    }));

    const onView = (trackKey: string) => setViewingScreenShareTrackKey(trackKey);

    return (
      <div className="vs-stage font-mono text-xs">
        {/* ---------- header (compact, matches the rest of the app) ---------- */}
        <div
          className="flex items-center px-4 flex-shrink-0 h-[var(--bar-h)]"
          style={{ borderBottom: "1px solid var(--c-border)", color: "var(--c-text-muted)" }}
        >
          <button
            onClick={onBack}
            aria-label="Back"
            className="mr-3 inline-flex items-center gap-1 leading-none transition-colors text-[var(--c-text-muted)] hover:text-[var(--c-accent)]"
          >
            <ArrowLeft size={12} />
          </button>
          <span style={{ flex: 1, color: "var(--c-accent)" }} className="flex items-center gap-1.5">
            <Volume2 size={12} />
            {channelName}
          </span>
          {headerActions && <div className="flex items-center gap-2">{headerActions}</div>}
        </div>

        {/* ---------- body ---------- */}
        <div className="vs-body">
          {sharePicking ? (
            <ScreenSharePicker />
          ) : !isInCall ? (
            <div className="vs-preview" data-testid="voice-channel-observers">
              {previewPeople.length === 0 ? (
                // Empty: label + CTA centered together as one group.
                <div className="vs-preview-empty">
                  <span className="vs-preview-empty-label">No one in this channel</span>
                  <Button data-testid="voice-join-cta" variant="primary" onClick={onJoin}>
                    Join Voice
                  </Button>
                </div>
              ) : (
                // Populated: roster, then the CTA pinned below the users.
                <>
                  <NavigableGrid<StageParticipant>
                    items={previewPeople}
                    getKey={(p) => p.identity}
                    autoFocus={false}
                    minCellWidth={180}
                    maxCellWidth={240}
                    renderCell={(p, { focused: f }) => (
                      <StageTile participant={p} mode="preview" focused={f} onView={onView} />
                    )}
                  />
                  <div className="vs-preview-cta">
                    <Button data-testid="voice-join-cta" variant="primary" onClick={onJoin}>
                      Join Voice
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
                  <div className="vs-tile vs-empty">no active stream</div>
                )}
              </div>
              <div className="vs-film">
                {people
                  .filter((p) => !focused || p.identity !== focused.identity)
                  .map((p) => (
                    <div key={p.identity} className="vs-film-cell">
                      <StageTile participant={p} mode="film" onFocus={setFocusId} onView={onView} />
                    </div>
                  ))}
              </div>
            </div>
          ) : (
            <NavigableGrid<StageParticipant>
              items={people}
              getKey={(p) => p.identity}
              testId="voice-channel-view"
              emptyLabel="Connecting…"
              autoFocus={false}
              minCellWidth={180}
              maxCellWidth={240}
              renderCell={(p, { focused: f }) => (
                <StageTile participant={p} mode="grid" focused={f} onView={onView} />
              )}
            />
          )}
        </div>

        {/* ---------- footer (fixed height) ---------- */}
        {footer ? (
          footer
        ) : (
          <div className="vs-foot">
            <div className="vs-foot-side">
              {isInCall ? (
                <div className="vs-tray">
                  <button
                    className={"vs-tray-btn danger" + (micMuted ? " on" : "")}
                    data-testid="voice-tray-mute"
                    title={micMuted ? "Unmute" : "Mute"}
                    aria-label={micMuted ? "Unmute microphone" : "Mute microphone"}
                    onClick={() => voiceSession.toggleMute()}
                  >
                    {micMuted ? <MicOff size={15} /> : <Mic size={15} />}
                  </button>
                  <button
                    className={"vs-tray-btn" + (shareActive ? " on" : "")}
                    data-testid="voice-tray-screenshare"
                    title={shareActive ? "Stop screen share" : shareInFlight ? "Cancel (recover)" : "Share screen"}
                    aria-label={shareActive ? "Stop screen share" : "Share screen"}
                    onClick={() => toggleScreenShare(share)}
                  >
                    {shareActive ? <MonitorOff size={15} /> : <Monitor size={15} />}
                  </button>
                  <div className="vs-tray-sep" />
                  <button
                    className="vs-tray-leave"
                    data-testid="voice-tray-leave"
                    title="Leave channel"
                    aria-label="Leave voice channel"
                    onClick={onLeave}
                  >
                    {/* Exit arrow points left (mirrored) — a "leave" gesture. */}
                    <LogOut size={14} style={{ transform: "scaleX(-1)" }} /> Leave
                  </button>
                </div>
              ) : (
                <Button data-testid="voice-join-leave-button" variant="primary" size="sm" className="h-[1.75rem]" onClick={onJoin}>
                  <Phone size={14} /> Join
                </Button>
              )}
            </div>
            <div className="vs-foot-side right">
              <button
                className="flex items-center gap-2 text-xs transition-colors text-[var(--c-text-muted)] hover:text-[var(--c-text)]"
                data-testid="voice-settings-link"
                onClick={onOpenSettings}
              >
                <SlidersHorizontal size={15} /> Voice Settings
              </button>
            </div>
          </div>
        )}
      </div>
    );
  },
);
