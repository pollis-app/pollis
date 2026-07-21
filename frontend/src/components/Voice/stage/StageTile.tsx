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

/** The single media surface a tile renders. A tile shows EXACTLY one of these —
 *  camera and screenshare are separate tiles, never crammed into one slot, so
 *  the old "both track keys set, one silently dropped" state is unrepresentable.
 *  A participant publishing camera + screenshare at once (#394) yields two tiles
 *  (`{kind:'camera'}` + `{kind:'screenshare'}`); see `tilesFor` in VoiceStage. */
export type TileMedia =
  | { kind: "audio" }
  | { kind: "camera"; trackKey: string; width?: number; height?: number }
  | { kind: "screenshare"; trackKey: string; width?: number; height?: number };

export interface StageTileModel {
  /** Unique per tile: `${identity}` (audio), `${identity}:cam`, or
   *  `${identity}:screen`. Distinct from `identity` because one participant can
   *  own two tiles. */
  tileKey: string;
  /** Owning participant — drives name, mute/speaking, per-user volume. Shared by
   *  a participant's camera and screenshare tiles. */
  identity: string;
  name: string;
  avatarKey: string | null;
  isMuted: boolean;
  isLocal: boolean;
  isSpeaking: boolean;
  connectionQuality?: VoiceConnectionQuality;
  /** Local user only, while the voice session is still negotiating. */
  isConnecting?: boolean;
  media: TileMedia;
}

interface Props {
  participant: StageTileModel;
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
  const media = p.media;

  // A tile carries video when its media is a camera OR a screenshare — the two
  // are treated identically at the container level (#394): both are
  // spotlightable, fullscreenable, and carry the LIVE + res·fps chrome. The
  // pixels' source makes no difference to the container. An audio tile shows the
  // avatar + meter.
  const videoTrack =
    media.kind === "camera" || media.kind === "screenshare"
      ? { trackKey: media.trackKey, width: media.width, height: media.height }
      : null;
  const isVideo = videoTrack !== null;
  // A video tile in the filmstrip/grid is clickable to spotlight it; the big
  // tile is already focused, the preview state isn't a live call.
  const focusable = isVideo && !big && !preview;

  const stats = useScreenShareStats(videoTrack?.trackKey ?? null);
  const statsLabel = (() => {
    if (!videoTrack) {
      return null;
    }
    const w = stats.dimensions?.width ?? videoTrack.width;
    const h = stats.dimensions?.height ?? videoTrack.height;
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
      data-testid={`voice-tile-${p.tileKey}`}
      onClick={focusable ? () => onFocus?.(p.tileKey) : undefined}
    >
      <div className="vs-tile-stage">
        {media.kind === "screenshare" || media.kind === "camera" ? (
          <RemoteVideoTile
            trackKey={media.trackKey}
            initialWidth={media.width}
            initialHeight={media.height}
            preview={!big}
          />
        ) : (
          <div className="vs-tile-center">
            <Avatar
              avatarKey={p.avatarKey}
              size={big ? 92 : 60}
              alt={p.name}
              testId={`voice-tile-avatar-${p.tileKey}`}
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
            data-testid={`voice-tile-connecting-${p.tileKey}`}
            className="flex items-center leading-none"
            title="Connecting…"
            aria-label="Connecting"
          >
            <LoadingSpinner size="sm" />
          </span>
        ) : (
          <span
            className="vs-tag vs-pad6"
            data-testid={`voice-tile-quality-${p.tileKey}`}
            title={`Connection: ${p.connectionQuality ?? "excellent"}`}
          >
            <span className={"vs-sig " + signalClass(p.connectionQuality)}>
              <i /><i /><i /><i />
            </span>
          </span>
        )}
      </div>

      {/* LIVE badge: only useful before you join — once you're in the
          channel the video itself makes it obvious who's streaming. */}
      {isVideo && preview && (
        <div className="vs-bl"><span className="vs-tag live">LIVE</span></div>
      )}

      {/* res · fps badge on any in-call video tile (camera or screenshare). The
          e2e suite tells camera from screenshare by the tile's `:cam`/`:screen`
          key suffix, not this badge. */}
      {isVideo && !preview && statsLabel && (
        <div className="vs-br">
          {/* Machine-facing metric (res · fps) — stays monospace in BOTH
              skins. Without font-machine the refined skin would swap the
              stage's inherited font-mono to sans (index.css §refined). */}
          <span
            className="vs-tag res font-machine"
            data-testid={`voice-tile-stream-stats-${p.tileKey}`}
          >
            {statsLabel}
          </span>
        </div>
      )}

      {/* hover controls — any video tile in-call (camera or screenshare) is
          fullscreenable and spotlightable. */}
      {isVideo && !preview && videoTrack && (
        <div className="vs-hover">
          <button
            className="vs-hbtn"
            title="fullscreen"
            aria-label="Open fullscreen"
            onClick={(e) => { e.stopPropagation(); onView?.(videoTrack.trackKey); }}
          >
            <Maximize size={17} />
          </button>
          {focusable && (
            <button
              className="vs-hbtn"
              title="spotlight"
              aria-label="Spotlight this video"
              onClick={(e) => { e.stopPropagation(); onFocus?.(p.tileKey); }}
            >
              <Pin size={17} />
            </button>
          )}
        </div>
      )}

      {/* volume — remote only, and in-call only. While previewing the channel
          (not joined) you're not listening to anyone, so a per-user output
          volume control is meaningless; it appears on hover/focus once you've
          joined. Keyed by identity (per-user output gain), so a participant's
          camera and screenshare tiles both control the same volume. */}
      {!p.isLocal && !preview && (
        <div
          className="vs-vol"
          data-testid={`voice-tile-volume-${p.tileKey}`}
          onClick={(e) => e.stopPropagation()}
        >
          <RemoteUserVolumeSlider identity={p.identity} participantName={p.name} />
        </div>
      )}
    </div>
  );
};
