import React, { useRef } from "react";
import { useTranslation } from "react-i18next";
import { Play, Pause } from "lucide-react";
import { formatDuration } from "../../utils/format";
import { useAudioPlayback } from "../../hooks/useAudioPlayback";
import { usePressActivation, type ActivateOn } from "../../hooks/usePressActivation";

interface InlineAudioPlayerProps {
  src: string;
  title?: string;
  className?: string;
  autoPlay?: boolean;
  onClick?: () => void;
  /** `"press"`: a mouse press on the card runs `onClick` on pointer-down (#1529). */
  activateOn?: ActivateOn;
}

export const InlineAudioPlayer: React.FC<InlineAudioPlayerProps> = ({
  src,
  title,
  className = "",
  autoPlay = false,
  onClick,
  activateOn,
}) => {
  const { t } = useTranslation("common");
  const audioRef = useRef<HTMLAudioElement>(null);
  const press = usePressActivation<HTMLDivElement>(onClick, activateOn);
  const playback = useAudioPlayback(audioRef);
  const { isPlaying, currentTime, duration } = playback;

  const togglePlay = (e: React.MouseEvent) => {
    e.stopPropagation();
    playback.togglePlay();
  };

  const handleSeek = (e: React.ChangeEvent<HTMLInputElement>) => {
    e.stopPropagation();
    playback.seek(parseFloat(e.target.value));
  };

  return (
    <div
      className={`flex items-center gap-3 p-2 rounded-lg border-2 border-line bg-surface-high ${className}`}
      style={{
        cursor: onClick ? "pointer" : "default",
      }}
      onClick={press.onClick}
      onPointerDown={press.onPointerDown}
      onMouseDown={press.onMouseDown}
    >
      <audio ref={audioRef} src={src} autoPlay={autoPlay} preload="metadata" />

      <button
        onClick={togglePlay}
        className="p-1 rounded transition-colors duration-200 text-accent focus:outline-none focus:ring-2 focus:ring-accent focus:ring-offset-2 focus:ring-offset-black"
        aria-label={isPlaying ? t("media.pause") : t("media.play")}
      >
        {isPlaying ? <Pause className="w-4 h-4" /> : <Play className="w-4 h-4" />}
      </button>

      {title && (
        <span className="font-mono text-xs truncate min-w-0 text-accent">
          {title}
        </span>
      )}

      <input
        type="range"
        min={0}
        max={duration || 0}
        value={currentTime}
        onChange={handleSeek}
        onClick={(e) => e.stopPropagation()}
        className="flex-1 h-1 rounded appearance-none cursor-pointer accent-slider focus:outline-none focus:ring-2 focus:ring-accent focus:ring-offset-2 focus:ring-offset-black"
        disabled={!duration}
        aria-label={t("media.seek")}
      />

      <span className="font-mono text-xs whitespace-nowrap text-dim">
        {formatDuration(currentTime)} / {formatDuration(duration)}
      </span>
    </div>
  );
};
