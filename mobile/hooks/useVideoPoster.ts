// A poster frame for a local video file (#1585): the composer's card for a
// picked video shows its first frame, as desktop's does. expo-image cannot
// decode video, so a short-lived expo-video player grabs one thumbnail
// (a native image ref expo-image draws directly) and is released at once.
// Only ever a picked, local `file://` — never a decrypted attachment.
//
// The frame is asked for once the player has loaded the file: on iOS the
// player item is attached asynchronously, and a request before that returns
// nothing.

import { useEffect, useState } from "react";
import { createVideoPlayer, type VideoThumbnail } from "expo-video";

// Give up on a file the player never loads; the card keeps its plain surface.
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
          // No poster: the card keeps its plain surface under the play glyph.
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
      // An unmount before the frame came back: drop the player now.
      if (subscription) {
        release();
      }
    };
  }, [uri]);

  return poster;
}
