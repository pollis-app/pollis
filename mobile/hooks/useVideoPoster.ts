// A picked local video's first frame (#1585) via a short-lived expo-video player, asked for only once loaded (iOS).

import { useEffect, useState } from "react";
import { createVideoPlayer, type VideoThumbnail } from "expo-video";

const LOAD_TIMEOUT_MS = 8000;

export function useVideoPoster(uri: string | null): VideoThumbnail | null {
  const [poster, setPoster] = useState<VideoThumbnail | null>(null);

  useEffect(() => {
    setPoster(null);
    if (!uri) {
      return;
    }
    let player: ReturnType<typeof createVideoPlayer>;
    try {
      player = createVideoPlayer({ uri });
      player.muted = true;
    } catch {
      return;
    }
    let active = true;
    let released = false;
    let subscription: { remove: () => void } | null = null;
    let timer: ReturnType<typeof setTimeout> | null = null;
    const release = () => {
      if (released) {
        return;
      }
      released = true;
      subscription?.remove();
      if (timer) {
        clearTimeout(timer);
      }
      player.release();
    };
    const grab = () => {
      subscription?.remove();
      subscription = null;
      if (timer) {
        clearTimeout(timer);
        timer = null;
      }
      player
        .generateThumbnailsAsync(0, { maxWidth: 360, maxHeight: 360 })
        .then((thumbs) => {
          if (active && thumbs[0]) {
            setPoster(thumbs[0]);
          }
        })
        .catch(() => {
          // No poster: the card keeps its plain surface.
        })
        .finally(release);
    };

    if (player.status === "readyToPlay") {
      grab();
    } else {
      subscription = player.addListener("statusChange", ({ status }) => {
        if (status === "readyToPlay") {
          grab();
        } else if (status === "error") {
          release();
        }
      });
      timer = setTimeout(release, LOAD_TIMEOUT_MS);
    }
    return () => {
      active = false;
      if (subscription) {
        release();
      }
    };
  }, [uri]);

  return poster;
}
