// Resolves an attachment to a local `file://` URI and releases it on unmount (#346). `null` no-ops.
// A file still on disk (held elsewhere, or just released) is answered on the first render, so a
// remount (a Vault tab switch, a row scrolled back) draws the picture with no placeholder.
// A failure is never sticky (#1826): the tile recovers when anyone else resolves the same file.

import { useEffect, useState } from "react";
import {
  mediaCacheGeneration,
  peekMediaUri,
  resolveMediaUri,
  releaseMediaUri,
  subscribeMediaResolved,
} from "../lib/media/cache";
import { startMediaTile, type MediaTileState } from "../lib/media/tile";
import type { MessageAttachment } from "../types";

export type MediaUriState = MediaTileState;

function sameState(a: MediaUriState, b: MediaUriState): boolean {
  return a.uri === b.uri && a.loading === b.loading && a.error === b.error;
}

export function useMediaUri(
  attachment: Pick<
    MessageAttachment,
    "object_key" | "content_hash" | "content_type" | "sha256" | "key_scope"
  > | null,
): MediaUriState {
  const [state, setState] = useState<MediaUriState>(() => {
    const cached = attachment?.object_key ? peekMediaUri(attachment.content_hash) : null;
    return { uri: cached, loading: !!attachment && !cached, error: null };
  });

  // Re-resolve only when the bytes change: content_hash is the identity.
  const contentHash = attachment?.content_hash ?? null;
  const objectKey = attachment?.object_key ?? null;

  useEffect(() => {
    if (!attachment || !objectKey || !contentHash) {
      setState({ uri: null, loading: false, error: null });
      return;
    }

    return startMediaTile(
      contentHash,
      {
        peek: peekMediaUri,
        resolve: () => resolveMediaUri(attachment),
        release: (resolvedIn) => {
          void releaseMediaUri(contentHash, resolvedIn);
        },
        generation: mediaCacheGeneration,
        onResolved: subscribeMediaResolved,
      },
      (next) => setState((prev) => (sameState(prev, next) ? prev : next)),
    );
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [contentHash, objectKey]);

  return state;
}
