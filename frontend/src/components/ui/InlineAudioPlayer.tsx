import React, { useState, useRef, useEffect } from "react";
import { useTranslation } from "react-i18next";
import { Play, Pause } from "lucide-react";
import { formatDuration } from "../../utils/format";
import { usePressActivation, type ActivateOn } from "../../hooks/usePressActivation";

interface InlineAudioPlayerProps {
  src: string;
  title?: string;
  className?: string;
  autoPlay?: boolean;
  onClick?: () => void;
  /** `"press"`: a plain mouse press on the card (not on its play button or
      seek bar) runs `onClick` on pointer-down (#1529). */
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
  const [isPlaying, setIsPlaying] = useState(false);
  const [currentTime, setCurrentTime] = useState(0);
  const [duration, setDuration] = useState(0);
  const audioRef = useRef<HTMLAudioElement>(null);
  const press = usePressActivation<HTMLDivElement>(onClick, activateOn);

  useEffect(() => {
    const audio = audioRef.current;
    if (!audio) {
      return;
    }

    const updateTime = () => setCurrentTime(audio.currentTime);
    const updateDuration = () => {
      if (isFinite(audio.duration)) {
        setDuration(audio.duration);
      }
    };
    const handlePlay = () => setIsPlaying(true);
    const handlePause = () => setIsPlaying(false);

    audio.addEventListener("timeupdate", updateTime);
    audio.addEventListener("loadedmetadata", updateDuration);
    audio.addEventListener("durationchange", updateDuration);
    audio.addEventListener("play", handlePlay);
    audio.addEventListener("pause", handlePause);

    return () => {
      audio.removeEventListener("timeupdate", updateTime);
      audio.removeEventListener("loadedmetadata", updateDuration);
      audio.removeEventListener("durationchange", updateDuration);
      audio.removeEventListener("play", handlePlay);
      audio.removeEventListener("pause", handlePause);
    };
  }, []);

  const togglePlay = (e: React.MouseEvent) => {
    e.stopPropagation();
    if (!audioRef.current) {
      return;
    }
    if (isPlaying) {
      audioRef.current.pause();
    } else {
      audioRef.current.play();
    }
  };

  const handleSeek = (e: React.ChangeEvent<HTMLInputElement>) => {
    e.stopPropagation();
    if (!audioRef.current) {
      return;
    }
    const newTime = parseFloat(e.target.value);
    audioRef.current.currentTime = newTime;
    setCurrentTime(newTime);
  };

  return (
    <div
      className={`flex items-center gap-3 p-2 rounded-lg border-2 border-line bg-surface-high ${className}`}
      style={{
        cursor: onClick ? "pointer" : "default",
      }}
      onClick={press.onClick}
      // The play button and the seek bar are their own controls: a press on
      // them never opens the viewer (usePressActivation leaves nested
      // controls alone).
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
