// A poster frame for a local video file (#1585): the composer's card for a
// picked video shows its first frame, as desktop's does. expo-image cannot
// decode video, so a short-lived expo-video player grabs one thumbnail
// (a native image ref expo-image draws directly) and is released at once.
// Only ever a picked, local `file://` — never a decrypted attachment.

import { useEffect, useState } from "react";
import { createVideoPlayer, type VideoThumbnail } from "expo-video";

export function useVideoPoster(uri: string | null): VideoThumbnail | null {
  const [poster, setPoster] = useState<VideoThumbnail | null>(null);

  useEffect(() => {
    setPoster(null);
    if (!uri) {
      return;
    }
    let active = true;
    let player: ReturnType<typeof createVideoPlayer> | null = null;
    try {
      player = createVideoPlayer({ uri });
      player.muted = true;
    } catch {
      return;
    }
    const owned = player;
    owned
      .generateThumbnailsAsync(0, { maxWidth: 360, maxHeight: 360 })
      .then((thumbs) => {
        if (active && thumbs[0]) {
          setPoster(thumbs[0]);
        }
      })
      .catch(() => {
        // No poster: the card keeps its plain surface under the play glyph.
      })
      .finally(() => {
        owned.release();
      });
    return () => {
      active = false;
    };
  }, [uri]);

  return poster;
}
