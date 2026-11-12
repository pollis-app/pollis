import { useState, useEffect, type RefObject } from "react";

/** Play state, position and duration of an `<audio>` element, shared by `AudioPlayer` and `InlineAudioPlayer`. */
export function useAudioPlayback(audioRef: RefObject<HTMLAudioElement | null>) {
  const [isPlaying, setIsPlaying] = useState(false);
  const [currentTime, setCurrentTime] = useState(0);
  const [duration, setDuration] = useState(0);

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
    // The ref object is stable, so this runs once per mount.
  }, [audioRef]);

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

  const seek = (newTime: number) => {
    if (!audioRef.current) {
      return;
    }
    audioRef.current.currentTime = newTime;
    setCurrentTime(newTime);
  };

  return { isPlaying, currentTime, duration, togglePlay, seek };
}
