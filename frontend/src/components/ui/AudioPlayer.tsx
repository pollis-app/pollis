import React, { useState, useRef } from "react";
import { useTranslation } from "react-i18next";
import { formatDuration } from "../../utils/format";
import { useAudioPlayback } from "../../hooks/useAudioPlayback";
import {
  Play,
  Pause,
  Volume2,
  VolumeX,
  SkipBack,
  SkipForward,
} from "lucide-react";

interface AudioPlayerProps {
  src: string;
  title?: string;
  className?: string;
  autoPlay?: boolean;
  loop?: boolean;
  preload?: "none" | "metadata" | "auto";
}

export const AudioPlayer: React.FC<AudioPlayerProps> = ({
  src,
  title,
  className = "",
  autoPlay = false,
  loop = false,
  preload = "metadata",
}) => {
  const { t } = useTranslation("common");
  const [volume, setVolume] = useState(1);
  const [isMuted, setIsMuted] = useState(false);
  const audioRef = useRef<HTMLAudioElement>(null);
  const { isPlaying, currentTime, duration, togglePlay, seek } = useAudioPlayback(audioRef);

  const handleSeek = (e: React.ChangeEvent<HTMLInputElement>) => {
    seek(parseFloat(e.target.value));
  };

  const handleVolumeChange = (e: React.ChangeEvent<HTMLInputElement>) => {
    if (!audioRef.current) {
      return;
    }
    const newVolume = parseFloat(e.target.value);
    setVolume(newVolume);
    audioRef.current.volume = newVolume;
    setIsMuted(newVolume === 0);
  };

  const toggleMute = () => {
    if (!audioRef.current) {
      return;
    }
    if (isMuted) {
      audioRef.current.volume = volume;
      setIsMuted(false);
    } else {
      audioRef.current.volume = 0;
      setIsMuted(true);
    }
  };

  const skipBackward = () => {
    if (!audioRef.current) {
      return;
    }
    audioRef.current.currentTime = Math.max(0, currentTime - 10);
  };

  const skipForward = () => {
    if (!audioRef.current) {
      return;
    }
    audioRef.current.currentTime = Math.min(duration, currentTime + 10);
  };

  return (
    <div
      className={`w-full p-4 rounded-md border-2 border-line bg-surface-high ${className}`}
    >
      <audio
        ref={audioRef}
        src={src}
        autoPlay={autoPlay}
        loop={loop}
        preload={preload}
      />

      {title && (
        <div className="mb-3">
          <h3 className="font-mono font-medium text-sm truncate text-accent">
            {title}
          </h3>
        </div>
      )}

      <div className="mb-4">
        <input
          type="range"
          min={0}
          max={duration || 0}
          value={currentTime}
          onChange={handleSeek}
          className="w-full h-2 rounded-lg appearance-none cursor-pointer accent-slider focus:outline-none focus:ring-2 focus:ring-accent focus:ring-offset-2 focus:ring-offset-black"
          disabled={!duration}
          aria-label={t("media.seek")}
        />
        <div className="flex justify-between text-xs font-mono mt-1 text-dim">
          <span>{formatDuration(currentTime)}</span>
          <span>{formatDuration(duration)}</span>
        </div>
      </div>

      <div className="flex items-center justify-between">
        <div className="flex items-center gap-2">
          <button
            onClick={skipBackward}
            className="p-2 rounded transition-colors duration-200 text-accent focus:outline-none focus:ring-2 focus:ring-accent focus:ring-offset-2 focus:ring-offset-black"
            aria-label={t("media.skipBackward")}
          >
            <SkipBack className="w-4 h-4" />
          </button>

          <button
            onClick={togglePlay}
            className="p-3 rounded transition-colors duration-200 text-accent focus:outline-none focus:ring-2 focus:ring-accent focus:ring-offset-2 focus:ring-offset-black"
            aria-label={isPlaying ? t("media.pause") : t("media.play")}
          >
            {isPlaying ? <Pause className="w-5 h-5" /> : <Play className="w-5 h-5" />}
          </button>

          <button
            onClick={skipForward}
            className="p-2 rounded transition-colors duration-200 text-accent focus:outline-none focus:ring-2 focus:ring-accent focus:ring-offset-2 focus:ring-offset-black"
            aria-label={t("media.skipForward")}
          >
            <SkipForward className="w-4 h-4" />
          </button>
        </div>

        <div className="flex items-center gap-2">
          <button
            onClick={toggleMute}
            className="p-2 rounded transition-colors duration-200 text-accent focus:outline-none focus:ring-2 focus:ring-accent focus:ring-offset-2 focus:ring-offset-black"
            aria-label={isMuted ? t("media.unmute") : t("media.mute")}
          >
            {isMuted ? <VolumeX className="w-4 h-4" /> : <Volume2 className="w-4 h-4" />}
          </button>

          <input
            type="range"
            min={0}
            max={1}
            step={0.1}
            value={isMuted ? 0 : volume}
            onChange={handleVolumeChange}
            className="w-20 h-2 rounded-lg appearance-none cursor-pointer accent-slider focus:outline-none focus:ring-2 focus:ring-accent focus:ring-offset-2 focus:ring-offset-black"
            aria-label={t("media.volume")}
          />
        </div>
      </div>
    </div>
  );
};
