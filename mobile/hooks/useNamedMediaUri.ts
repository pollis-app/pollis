// `useMediaUri`'s twin for the named copy (#1248); `enabled: false` takes no reference.
// Like it, a failure recovers when anyone else resolves the same file (#1826).

import { useEffect, useState } from "react";
import {
  mediaCacheGeneration,
  releaseNamedMediaUri,
  resolveNamedMediaUri,
  subscribeMediaResolved,
} from "../lib/media/cache";
import { startMediaTile, type MediaTileState } from "../lib/media/tile";
import type { MessageAttachment } from "../types";

export type NamedMediaUriState = MediaTileState;

export function useNamedMediaUri(
  attachment: Pick<
    MessageAttachment,
    "object_key" | "content_hash" | "content_type" | "filename" | "localPreviewUri" | "sha256" | "key_scope"
  > | null,
  enabled: boolean,
): NamedMediaUriState {
  const [state, setState] = useState<NamedMediaUriState>({
    uri: null,
    loading: !!attachment && enabled,
    error: null,
  });

  const contentHash = attachment?.content_hash ?? null;
  const objectKey = attachment?.object_key ?? null;
  const preview = attachment?.localPreviewUri ?? null;

  useEffect(() => {
    if (!attachment || !enabled || (!objectKey && !preview)) {
      setState({ uri: null, loading: false, error: null });
      return;
    }

    return startMediaTile(
      // A local preview never fails over the network: nothing to listen for.
      objectKey ? contentHash : null,
      {
        // The named copy is made per mount; no synchronous answer.
        peek: () => null,
        resolve: () => resolveNamedMediaUri(attachment),
        // The last release unlinks the named copy.
        release: (resolvedIn) => {
          void releaseNamedMediaUri(
            { object_key: objectKey ?? "", content_hash: contentHash ?? "" },
            resolvedIn,
          );
        },
        generation: mediaCacheGeneration,
        onResolved: subscribeMediaResolved,
      },
      setState,
    );
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [contentHash, objectKey, preview, enabled]);

  return state;
}
