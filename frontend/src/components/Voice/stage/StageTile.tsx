// One participant on the voice stage; fills its parent, which owns sizing.

import React, { useEffect, useRef } from "react";
import { useTranslation } from "react-i18next";
import { Mic, MicOff, Maximize, Pin } from "lucide-react";

import type { VoiceConnectionQuality } from "../../../types";
import { Avatar } from "../../ui/Avatar";
import { LoadingSpinner } from "../../ui/LoaderSpinner";
import { RemoteUserVolumeSlider } from "../RemoteUserVolumeSlider";
import { SignalBars } from "../SignalBars";
import { RemoteVideoTile } from "../RemoteVideoTile";
import { useScreenShareStats } from "../../../screenshare/useScreenShareStats";
import { audioLevels, BAND_COUNT } from "../../../voice/audioLevels";
import { probeRender } from "../../../utils/renderProbe";

/** Local level above which a muted user is "talking into a muted mic" (Discord's hint). */
const MUTED_TALK_LEVEL = 0.25;
/** How long the hint stays after the last loud frame, so it doesn't flicker between words. */
const MUTED_TALK_HOLD_MS = 1500;

// Live meter: per-band levels written as CSS variables via a ref, no React state. With
// `mutedLocal`, loud frames also toggle `data-talking-muted` on the tile (the DSP chain keeps the
// local meter running while muted; only the frames sent to the room are zeroed).
const LiveWaveform: React.FC<{ identity: string; mutedLocal?: boolean }> = ({ identity, mutedLocal = false }) => {
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const el = ref.current;
    if (!el) {
      return;
    }
    const tile = el.closest<HTMLElement>(".vs-tile");
    let hold: ReturnType<typeof setTimeout> | null = null;
    const clearHint = () => {
      tile?.removeAttribute("data-talking-muted");
      hold = null;
    };
    // A 0..1 scale factor: the bars animate `transform`, so a level change never relayouts.
    const apply = (bands: number[]) => {
      let peak = 0;
      for (let i = 0; i < BAND_COUNT; i++) {
        const level = bands[i] ?? 0;
        peak = Math.max(peak, level);
        const scale = Math.max(0.08, Math.round(level * 100) / 100);
        el.style.setProperty(`--eq${i}`, String(scale));
      }
      if (mutedLocal && peak >= MUTED_TALK_LEVEL && tile) {
        tile.setAttribute("data-talking-muted", "true");
        if (hold !== null) {
          clearTimeout(hold);
        }
        hold = setTimeout(clearHint, MUTED_TALK_HOLD_MS);
      }
    };
    const seed = audioLevels.get(identity);
    if (seed) {
      apply(seed);
    }
    const off = audioLevels.subscribe(identity, apply);
    return () => {
      off();
      if (hold !== null) {
        clearTimeout(hold);
      }
      clearHint();
    };
  }, [identity, mutedLocal]);

  return (
    <div className="vs-eq" ref={ref} aria-hidden="true">
      {Array.from({ length: BAND_COUNT }, (_, i) => (
        <i key={i} />
      ))}
    </div>
  );
};

export type TileMode = "grid" | "film" | "big" | "preview";

/** Exactly one media surface per tile; camera + screenshare are two tiles (#394). */
export type TileMedia =
  | { kind: "audio" }
  | { kind: "camera"; trackKey: string; width?: number; height?: number }
  | { kind: "screenshare"; trackKey: string; width?: number; height?: number };

export interface StageTileModel {
  /** Unique per tile; one participant can own two. */
  tileKey: string;
  /** Owning participant, shared by their camera and screenshare tiles. */
  identity: string;
  name: string;
  avatarKey: string | null;
  isMuted: boolean;
  isLocal: boolean;
  isSpeaking: boolean;
  connectionQuality?: VoiceConnectionQuality;
  isConnecting?: boolean;
  /** Local tile while LiveKit re-establishes the room. */
  isReconnecting?: boolean;
  /** Remote tile whose media is paused by our reconnect; drawn dimmed. */
  stale?: boolean;
  media: TileMedia;
}

interface Props {
  participant: StageTileModel;
  mode: TileMode;
  /** Keyboard focus from the grid, mirroring hover. */
  focused?: boolean;
  onFocus?: (identity: string) => void;
  onView?: (trackKey: string) => void;
}

const StageTileImpl: React.FC<Props> = ({
  participant: p,
  mode,
  focused = false,
  onFocus,
  onView,
}) => {
  probeRender("StageTile");
  // Per-tile count, so a spec can prove a change touched exactly one tile.
  probeRender(`StageTile:${p.tileKey}`);
  const { t } = useTranslation("voice");
  const big = mode === "big";
  const preview = mode === "preview";
  const media = p.media;

  // Camera and screenshare alike (#394).
  const videoTrack =
    media.kind === "camera" || media.kind === "screenshare"
      ? { trackKey: media.trackKey, width: media.width, height: media.height }
      : null;
  const isVideo = videoTrack !== null;
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
    p.stale && "stale",
  ]
    .filter(Boolean)
    .join(" ");

  const micLabel = p.isMuted
    ? t("tile.micMuted")
    : p.isSpeaking
      ? t("tile.micSpeaking")
      : t("tile.micOpen");

  return (
    <div
      className={cls}
      data-testid={`voice-tile-${p.tileKey}`}
      // A video tile is a button that spotlights itself; audio tiles are plain regions.
      role={focusable ? "button" : undefined}
      tabIndex={focusable ? 0 : undefined}
      aria-label={focusable ? t("tile.spotlightNamed", { name: p.name }) : undefined}
      onClick={focusable ? () => onFocus?.(p.tileKey) : undefined}
      onKeyDown={
        focusable
          ? (e) => {
              if (e.key === "Enter" || e.key === " ") {
                e.preventDefault();
                onFocus?.(p.tileKey);
              }
            }
          : undefined
      }
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
            {!preview && <LiveWaveform identity={p.identity} mutedLocal={p.isLocal && p.isMuted} />}
            {p.isLocal && p.isMuted && !preview && (
              // Shown by CSS only while `data-talking-muted` is set (speech into a muted mic).
              <span className="vs-muted-hint" data-testid={`voice-tile-muted-hint-${p.tileKey}`} role="status">
                {t("tile.mutedHint")}
              </span>
            )}
          </div>
        )}
      </div>

      <div className="vs-tl">
        <span className={"vs-name" + (p.isLocal ? " host" : "")}>
          {p.isMuted ? (
            <span className="vs-ic danger" role="img" aria-label={micLabel} title={micLabel}><MicOff size={13} /></span>
          ) : (
            <span className={"vs-ic" + (p.isSpeaking ? " accent" : "")} role="img" aria-label={micLabel} title={micLabel}><Mic size={13} /></span>
          )}
          <bdi className="vs-nm" title={p.name}>{p.name}</bdi>
        </span>
      </div>

      <div className="vs-tr">
        {p.isConnecting || p.isReconnecting ? (
          <span
            data-testid={`voice-tile-${p.isReconnecting ? "reconnecting" : "connecting"}-${p.tileKey}`}
            className="vs-tag flex items-center gap-1.5 leading-none"
            title={p.isReconnecting ? t("tile.reconnectingTitle") : t("tile.connectingTitle")}
            aria-label={p.isReconnecting ? t("tile.reconnectingLabel") : t("tile.connectingLabel")}
          >
            <LoadingSpinner size="sm" />
            {p.isReconnecting ? t("stage.reconnecting") : null}
          </span>
        ) : (
          <SignalBars
            quality={p.connectionQuality}
            testId={`voice-tile-quality-${p.tileKey}`}
            className="vs-pad6"
          />
        )}
      </div>

      {/* LIVE badge only before joining. */}
      {isVideo && preview && (
        <div className="vs-bl"><span className="vs-tag live">{t("tile.liveBadge")}</span></div>
      )}

      {/* e2e tells camera from screen by the `:cam`/`:screen` key, not this badge. */}
      {isVideo && !preview && statsLabel && (
        <div className="vs-br">
          {/* Machine metric: `font-machine` keeps it mono in refined. */}
          <span
            className="vs-tag res font-machine"
            data-testid={`voice-tile-stream-stats-${p.tileKey}`}
          >
            {statsLabel}
          </span>
        </div>
      )}

      {isVideo && !preview && videoTrack && (
        <div className="vs-hover">
          <button
            className="vs-hbtn"
            title={t("tile.fullscreenTitle")}
            aria-label={t("tile.fullscreenLabel")}
            onClick={(e) => { e.stopPropagation(); onView?.(videoTrack.trackKey); }}
          >
            <Maximize size={17} />
          </button>
          {focusable && (
            <button
              className="vs-hbtn"
              title={t("tile.spotlightTitle")}
              aria-label={t("tile.spotlightLabel")}
              onClick={(e) => { e.stopPropagation(); onFocus?.(p.tileKey); }}
            >
              <Pin size={17} />
            </button>
          )}
        </div>
      )}

      {/* Remote, in-call only; keyed by identity, so both tiles share it. */}
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

/** Memoised: `VoiceStage` hands every tile a reference-stable model, so a speaking event re-renders one tile. */
export const StageTile = React.memo(StageTileImpl);
