// The poster for a received video whose sender sent no `pv` (an older build, #1837): the one
// this device saved (#1894), else a frame decoded here and saved, so the file is fetched and
// decoded once rather than on every render. `decode` is false when the file is too big to fetch
// for a frame; a saved poster is still read then.

import { useEffect, useState } from "react";
import { invoke } from "../lib/native";
import {
  localVideoPoster,
  type LocalVideoPoster,
  type SavedVideoPoster,
} from "../lib/media/localVideoPoster";
import { mediaCacheGeneration, releaseNamedMediaUri, resolveNamedMediaUri } from "../lib/media/cache";
import { videoFrame } from "../lib/media/videoFrame";
import type { MessageAttachment } from "../types";

type PosterAttachment = Pick<
  MessageAttachment,
  "object_key" | "content_hash" | "content_type" | "filename" | "localPreviewUri" | "sha256" | "key_scope" | "poster"
>;

export function useLocalVideoPoster(
  attachment: PosterAttachment,
  enabled: boolean,
  decode: boolean,
): LocalVideoPoster | null {
  const [found, setFound] = useState<{ hash: string; poster: LocalVideoPoster } | null>(null);
  const hash = attachment.content_hash;

  useEffect(() => {
    if (!enabled || attachment.poster || !attachment.object_key) {
      return;
    }
    let active = true;
    void localVideoPoster(attachment, {
      get: (contentHash) => invoke<SavedVideoPoster | null>("get_local_video_poster", { contentHash }),
      save: (contentHash, frame) => invoke<SavedVideoPoster>("save_local_video_poster", { contentHash, frame }),
      decode: async () => {
        if (!decode) {
          return null;
        }
        // The named copy: AVFoundation, like AVPlayer, types a file by its extension.
        const resolvedIn = mediaCacheGeneration();
        try {
          return await videoFrame(await resolveNamedMediaUri(attachment));
        } catch {
          return null;
        } finally {
          void releaseNamedMediaUri(attachment, resolvedIn);
        }
      },
    }).then((poster) => {
      if (active && poster) {
        setFound({ hash, poster });
      }
    });
    return () => {
      active = false;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [hash, attachment.object_key, attachment.poster, enabled, decode]);

  // A poster found for an earlier video never shows for this one.
  return found && found.hash === hash ? found.poster : null;
}
