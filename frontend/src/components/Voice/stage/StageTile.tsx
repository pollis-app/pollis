// One participant on the voice stage. Flat amber-POSIX terminal tile:
// name + live mic state (top-left), connection-quality bars (top-right),
// a centered avatar with a live multi-band audio meter when audio-only, or
// the screenshare feed (with LIVE + res·fps badges) when streaming.
// Speaking flips the border to a solid accent — no glow. Motion is minimal
// and informational: only the meter moves, driven by real per-source levels.
//
// The tile fills its parent (a NavigableGrid cell, a filmstrip cell, or
// the spotlight main area), so callers own the sizing/position logic.
//
// Reuses the app's own building blocks: Avatar, RemoteVideoTile (the real
// screenshare renderer), RemoteUserVolumeSlider (the persisted per-user
// mixer volume), and useScreenShareStats. lucide icons throughout.

import React, { useEffect, useRef } from "react";
import { Mic, MicOff, Maximize, Pin } from "lucide-react";

import type { VoiceConnectionQuality } from "../../../types";
import { Avatar } from "../../ui/Avatar";
import { LoadingSpinner } from "../../ui/LoaderSpinner";
import { RemoteUserVolumeSlider } from "../RemoteUserVolumeSlider";
import { RemoteVideoTile } from "../RemoteVideoTile";
import { useScreenShareStats } from "../../../screenshare/useScreenShareStats";
import { audioLevels, BAND_COUNT } from "../../../voice/audioLevels";

// Live audio meter: BAND_COUNT bars whose heights track this source's
// real per-band levels (pushed from Rust at ~20 Hz). Subscribes by
// identity and writes CSS variables straight onto the bar container via a
// ref — no React state, no re-render. Falls back to the static glyph
// (CSS default heights) until/if levels arrive. Mounted only for in-call
// audio-only tiles, so the subscription lifecycle matches the meter's.
const LiveWaveform: React.FC<{ identity: string }> = ({ identity }) => {
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const el = ref.current;
    if (!el) {
      return;
    }
    const apply = (bands: number[]) => {
      for (let i = 0; i < BAND_COUNT; i++) {
        // Floor at a small height so a silent bar still reads as a bar.
        const pct = Math.max(8, Math.round((bands[i] ?? 0) * 100));
        el.style.setProperty(`--eq${i}`, `${pct}%`);
      }
    };
    const seed = audioLevels.get(identity);
    if (seed) {
      apply(seed);
    }
    return audioLevels.subscribe(identity, apply);
  }, [identity]);

  return (
    <div className="vs-eq" ref={ref} aria-hidden="true">
      {Array.from({ length: BAND_COUNT }, (_, i) => (
        <i key={i} />
      ))}
    </div>
  );
};

export type TileMode = "grid" | "film" | "big" | "preview";

export interface StageParticipant {
  identity: string;
  name: string;
  avatarKey: string | null;
  isMuted: boolean;
  isLocal: boolean;
  isSpeaking: boolean;
  connectionQuality?: VoiceConnectionQuality;
  /** Track key + hint dimensions of this participant's active screen
   *  share (local preview or remote). Undefined ⇒ audio-only layout. */
  streamTrackKey?: string;
  streamWidth?: number;
  streamHeight?: number;
  /** Track key + hint dimensions of this participant's active webcam (local
   *  preview or remote). Renders as the tile's face — the avatar's slot —
   *  when present and there's no screen share occupying the tile. */
  cameraTrackKey?: string;
  cameraWidth?: number;
  cameraHeight?: number;
  /** Local user only, while the voice session is still negotiating. */
  isConnecting?: boolean;
}

interface Props {
  participant: StageParticipant;
  mode: TileMode;
  /** Keyboard focus from the enclosing NavigableGrid — reveals the volume
   *  strip and highlights the border, mirroring mouse hover. */
  focused?: boolean;
  /** Focus this streamer in the inline spotlight. */
  onFocus?: (identity: string) => void;
  /** Open the global fullscreen viewer for a track. */
  onView?: (trackKey: string) => void;
}

// LiveKit quality → the design's 3-step signal scale.
function signalClass(q: VoiceConnectionQuality | undefined): string {
  switch (q) {
    case "poor":
    case "lost":
      return "poor";
    case "good":
      return "fair";
    default:
      return "good";
  }
}

export const StageTile: React.FC<Props> = ({
  participant: p,
  mode,
  focused = false,
  onFocus,
  onView,
}) => {
  const big = mode === "big";
  const preview = mode === "preview";

  const hasFeed = p.streamTrackKey !== undefined;
  // Camera shows as the tile face only when a screen share isn't already
  // occupying it — a participant doing both surfaces the screen here and
  // their camera follows the spotlight/screenshare; the tile face stays the
  // screen. Camera never drives the spotlight (that's screen-share only).
  const hasCamera = !hasFeed && p.cameraTrackKey !== undefined;
  // A streaming tile in the filmstrip is clickable to spotlight it; the
  // big tile is already focused, the preview state isn't a live call.
  const focusable = hasFeed && !big && !preview;

  const stats = useScreenShareStats(p.streamTrackKey ?? null);
  const statsLabel = (() => {
    if (!hasFeed) {
      return null;
    }
    const w = stats.dimensions?.width ?? p.streamWidth;
    const h = stats.dimensions?.height ?? p.streamHeight;
    if (!w || !h) {
      return null;
    }
    const heightLabel = h % 90 === 0 && h <= 4320 ? `${h}p` : `${h}px`;
    const fpsLabel = stats.fps > 0 ? `${stats.fps}fps` : "…";
    return `${heightLabel} · ${fpsLabel}`;
  })();

  const cls = [
    "vs-tile",
    p.isSpeaking && "speaking",
    focusable && "clickable",
    focused && "focused",
  ]
    .filter(Boolean)
    .join(" ");

  return (
    <div
      className={cls}
      data-testid={`voice-tile-${p.identity}`}
      onClick={focusable ? () => onFocus?.(p.identity) : undefined}
    >
      <div className="vs-tile-stage">
        {hasFeed ? (
          <RemoteVideoTile
            trackKey={p.streamTrackKey!}
            initialWidth={p.streamWidth}
            initialHeight={p.streamHeight}
            preview={!big}
          />
        ) : hasCamera ? (
          <RemoteVideoTile
            trackKey={p.cameraTrackKey!}
            initialWidth={p.cameraWidth}
            initialHeight={p.cameraHeight}
            preview={!big}
          />
        ) : (
          <div className="vs-tile-center">
            <Avatar
              avatarKey={p.avatarKey}
              size={big ? 92 : 60}
              alt={p.name}
              testId={`voice-tile-avatar-${p.identity}`}
            />
            {!preview && <LiveWaveform identity={p.identity} />}
          </div>
        )}
      </div>

      {/* top-left: name + mic state */}
      <div className="vs-tl">
        <span className={"vs-name" + (p.isLocal ? " host" : "")}>
          {p.isMuted ? (
            <span className="vs-ic danger"><MicOff size={13} /></span>
          ) : (
            <span className={"vs-ic" + (p.isSpeaking ? " accent" : "")}><Mic size={13} /></span>
          )}
          <span className="vs-nm" title={p.name}>{p.name}</span>
        </span>
      </div>

      {/* top-right: connecting spinner (local) → connection signal */}
      <div className="vs-tr">
        {p.isConnecting ? (
          <span
            data-testid={`voice-tile-connecting-${p.identity}`}
            className="flex items-center leading-none"
            title="Connecting…"
            aria-label="Connecting"
          >
            <LoadingSpinner size="sm" />
          </span>
        ) : (
          <span
            className="vs-tag vs-pad6"
            data-testid={`voice-tile-quality-${p.identity}`}
            title={`Connection: ${p.connectionQuality ?? "excellent"}`}
          >
            <span className={"vs-sig " + signalClass(p.connectionQuality)}>
              <i /><i /><i /><i />
            </span>
          </span>
        )}
      </div>

      {/* LIVE badge: only useful before you join — once you're in the
          channel the stream itself makes it obvious who's streaming. */}
      {hasFeed && preview && (
        <div className="vs-bl"><span className="vs-tag live">LIVE</span></div>
      )}

      {/* res · fps badge stays on the in-call tiles. */}
      {hasFeed && !preview && statsLabel && (
        <div className="vs-br">
          {/* Machine-facing metric (res · fps) — stays monospace in BOTH
              skins. Without font-machine the refined skin would swap the
              stage's inherited font-mono to sans (index.css §refined). */}
          <span
            className="vs-tag res font-machine"
            data-testid={`voice-tile-stream-stats-${p.identity}`}
          >
            {statsLabel}
          </span>
        </div>
      )}

      {/* hover controls — streaming tiles in-call only */}
      {hasFeed && !preview && (
        <div className="vs-hover">
          <button
            className="vs-hbtn"
            title="fullscreen"
            aria-label="Open fullscreen"
            onClick={(e) => { e.stopPropagation(); onView?.(p.streamTrackKey!); }}
          >
            <Maximize size={17} />
          </button>
          {focusable && (
            <button
              className="vs-hbtn"
              title="spotlight"
              aria-label="Spotlight this stream"
              onClick={(e) => { e.stopPropagation(); onFocus?.(p.identity); }}
            >
              <Pin size={17} />
            </button>
          )}
        </div>
      )}

      {/* volume — remote only, and in-call only. While previewing the channel
          (not joined) you're not listening to anyone, so a per-user output
          volume control is meaningless; it appears on hover/focus once you've
          joined. */}
      {!p.isLocal && !preview && (
        <div
          className="vs-vol"
          data-testid={`voice-tile-volume-${p.identity}`}
          onClick={(e) => e.stopPropagation()}
        >
          <RemoteUserVolumeSlider identity={p.identity} participantName={p.name} />
        </div>
      )}
    </div>
  );
};
