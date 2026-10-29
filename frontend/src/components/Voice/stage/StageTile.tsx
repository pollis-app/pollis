// One participant on the voice stage; fills its parent, which owns sizing.

import React, { useEffect, useRef } from "react";
import { useTranslation } from "react-i18next";
import { Mic, MicOff, Maximize, Pin } from "lucide-react";

import type { VoiceConnectionQuality } from "../../../types";
import { Avatar } from "../../ui/Avatar";
import { LoadingSpinner } from "../../ui/LoaderSpinner";
import { RemoteUserVolumeSlider } from "../RemoteUserVolumeSlider";
import { RemoteVideoTile } from "../RemoteVideoTile";
import { useScreenShareStats } from "../../../screenshare/useScreenShareStats";
import { audioLevels, BAND_COUNT } from "../../../voice/audioLevels";

// Live meter: per-band levels written as CSS variables via a ref, no React state.
const LiveWaveform: React.FC<{ identity: string }> = ({ identity }) => {
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const el = ref.current;
    if (!el) {
      return;
    }
    const apply = (bands: number[]) => {
      for (let i = 0; i < BAND_COUNT; i++) {
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
  const { t } = useTranslation("voice");
  // Literal keys so `scripts/i18n-check.mjs` resolves them; the enum is a wire value.
  const qualityLabels: Record<VoiceConnectionQuality, string> = {
    excellent: t("tile.qualityExcellent"),
    good: t("tile.qualityGood"),
    poor: t("tile.qualityPoor"),
    lost: t("tile.qualityLost"),
  };
  const qualityLabel = qualityLabels[p.connectionQuality ?? "excellent"];
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

      <div className="vs-tl">
        <span className={"vs-name" + (p.isLocal ? " host" : "")}>
          {p.isMuted ? (
            <span className="vs-ic danger"><MicOff size={13} /></span>
          ) : (
            <span className={"vs-ic" + (p.isSpeaking ? " accent" : "")}><Mic size={13} /></span>
          )}
          <bdi className="vs-nm" title={p.name}>{p.name}</bdi>
        </span>
      </div>

      <div className="vs-tr">
        {p.isConnecting ? (
          <span
            data-testid={`voice-tile-connecting-${p.tileKey}`}
            className="flex items-center leading-none"
            title={t("tile.connectingTitle")}
            aria-label={t("tile.connectingLabel")}
          >
            <LoadingSpinner size="sm" />
          </span>
        ) : (
          <span
            className="vs-tag vs-pad6"
            data-testid={`voice-tile-quality-${p.tileKey}`}
            title={t("tile.quality", { quality: qualityLabel })}
          >
            <span className={"vs-sig " + signalClass(p.connectionQuality)}>
              <i /><i /><i /><i />
            </span>
          </span>
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
