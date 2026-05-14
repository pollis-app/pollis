import React, { useState, useRef, useEffect } from "react";
import { formatDuration } from "../../utils/format";
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
  const [isPlaying, setIsPlaying] = useState(false);
  const [currentTime, setCurrentTime] = useState(0);
  const [duration, setDuration] = useState(0);
  const [volume, setVolume] = useState(1);
  const [isMuted, setIsMuted] = useState(false);
  const audioRef = useRef<HTMLAudioElement>(null);

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

  const togglePlay = () => {
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
    if (!audioRef.current) {
      return;
    }
    const newTime = parseFloat(e.target.value);
    audioRef.current.currentTime = newTime;
    setCurrentTime(newTime);
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

  const btnStyle: React.CSSProperties = { color: "var(--c-accent)" };

  return (
    <div
      className={`w-full p-4 rounded-md ${className}`}
      style={{
        border: "2px solid var(--c-border)",
        background: "var(--c-surface-high)",
      }}
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
          <h3
            className="font-mono font-medium text-sm truncate"
            style={{ color: "var(--c-accent)" }}
          >
            {title}
          </h3>
        </div>
      )}

      {/* Progress Bar */}
      <div className="mb-4">
        <input
          type="range"
          min={0}
          max={duration || 0}
          value={currentTime}
          onChange={handleSeek}
          className="w-full h-2 rounded-lg appearance-none cursor-pointer accent-slider focus:outline-none focus:ring-2 focus:ring-[var(--c-accent)] focus:ring-offset-2 focus:ring-offset-black"
          disabled={!duration}
          aria-label="Seek audio"
        />
        <div className="flex justify-between text-xs font-mono mt-1" style={{ color: "var(--c-text-dim)" }}>
          <span>{formatDuration(currentTime)}</span>
          <span>{formatDuration(duration)}</span>
        </div>
      </div>

      {/* Controls */}
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-2">
          <button
            onClick={skipBackward}
            className="p-2 rounded transition-colors duration-200 focus:outline-none focus:ring-2 focus:ring-[var(--c-accent)] focus:ring-offset-2 focus:ring-offset-black"
            style={btnStyle}
            aria-label="Skip backward 10 seconds"
          >
            <SkipBack className="w-4 h-4" />
          </button>

          <button
            onClick={togglePlay}
            className="p-3 rounded transition-colors duration-200 focus:outline-none focus:ring-2 focus:ring-[var(--c-accent)] focus:ring-offset-2 focus:ring-offset-black"
            style={btnStyle}
            aria-label={isPlaying ? "Pause" : "Play"}
          >
            {isPlaying ? <Pause className="w-5 h-5" /> : <Play className="w-5 h-5" />}
          </button>

          <button
            onClick={skipForward}
            className="p-2 rounded transition-colors duration-200 focus:outline-none focus:ring-2 focus:ring-[var(--c-accent)] focus:ring-offset-2 focus:ring-offset-black"
            style={btnStyle}
            aria-label="Skip forward 10 seconds"
          >
            <SkipForward className="w-4 h-4" />
          </button>
        </div>

        {/* Volume Control */}
        <div className="flex items-center gap-2">
          <button
            onClick={toggleMute}
            className="p-2 rounded transition-colors duration-200 focus:outline-none focus:ring-2 focus:ring-[var(--c-accent)] focus:ring-offset-2 focus:ring-offset-black"
            style={btnStyle}
            aria-label={isMuted ? "Unmute" : "Mute"}
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
            className="w-20 h-2 rounded-lg appearance-none cursor-pointer accent-slider focus:outline-none focus:ring-2 focus:ring-[var(--c-accent)] focus:ring-offset-2 focus:ring-offset-black"
            aria-label="Volume"
          />
        </div>
      </div>
    </div>
  );
};
